//! Pont vers l'éditeur Unreal (Devkit) par « Python Remote Execution » (LECTURE SEULE en 4a).
//!
//! Protocole: docs/ue-remote-execution.md. Une **session par appel d'outil**: découverte UDP
//! multicast, serveur TCP éphémère sur 127.0.0.1, `open_connection`, une `command`, lecture du
//! `command_result`, `close_connection`, tout fermé dans tous les cas. Pas de connexion persistante.
//!
//! Le code Python exécuté vient exclusivement des gabarits figés de `templates` (constantes); les
//! paramètres y entrent par `json.loads` d'un littéral, jamais par interpolation.

pub mod message;
#[cfg(any(test, feature = "test-support"))]
pub mod probes;
pub mod templates;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use message::{
    Message, MODE_EXEC_FILE, TYPE_CLOSE_CONNECTION, TYPE_COMMAND, TYPE_COMMAND_RESULT,
    TYPE_OPEN_CONNECTION, TYPE_PING, TYPE_PONG,
};

/// Nom du projet du Devkit Conan Exiles (`ConanSandbox.uproject`).
pub const DEFAULT_PROJECT_NAME: &str = "ConanSandbox";

const PING_INTERVAL: Duration = Duration::from_secs(1);
const UDP_POLL: Duration = Duration::from_millis(100);
const READ_CHUNK: usize = 8192;
/// Taille maximale acceptée pour une réponse (garde-fou contre un pair défaillant).
const MAX_REPLY_BYTES: usize = 64 * 1024 * 1024;

/// Échecs typés du pont.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum UeError {
    /// Aucun éditeur (du bon projet) n'a répondu à la découverte.
    #[error("Devkit injoignable: {0}")]
    DevkitNotRunning(String),
    #[error("erreur de protocole avec l'éditeur: {0}")]
    ProtocolError(String),
    #[error("délai dépassé pendant « {stage} » ({secs} s)")]
    Timeout { stage: &'static str, secs: f64 },
    /// L'éditeur a exécuté le script et signalé une erreur Python (traceback).
    #[error("erreur Python dans l'éditeur:\n{0}")]
    PythonError(String),
    /// Impossible d'ouvrir les sockets locaux.
    #[error("erreur réseau locale: {0}")]
    Socket(String),
    /// Refus local, sans rien envoyer: une commande précédente n'a pas abouti et l'éditeur est peut-être
    /// encore occupé (voir `ConanServer`, drapeau « suspect »).
    #[error("{0}")]
    EditorBusy(String),
}

/// Étape « exécution » d'une commande, pour les délais dépassés après l'envoi.
pub const STAGE_COMMAND: &str = "exécution de la commande dans l'éditeur";

impl UeError {
    pub fn code(&self) -> &'static str {
        match self {
            UeError::DevkitNotRunning(_) => "DevkitNotRunning",
            UeError::ProtocolError(_) => "ProtocolError",
            UeError::Timeout { .. } => "Timeout",
            UeError::PythonError(_) => "PythonError",
            UeError::Socket(_) => "SocketError",
            UeError::EditorBusy(_) => "EditorBusy",
        }
    }

    /// La commande est-elle peut-être encore en cours dans l'éditeur ? Vrai pour un délai dépassé
    /// pendant l'exécution et pour toute rupture de protocole (envoi, lecture, réponse illisible):
    /// on ne sait pas si Python tourne encore. Faux pour la découverte, la connexion, une erreur
    /// Python (l'éditeur a répondu) et les erreurs locales.
    pub fn command_in_doubt(&self) -> bool {
        match self {
            UeError::Timeout { stage, .. } => *stage == STAGE_COMMAND,
            UeError::ProtocolError(_) => true,
            _ => false,
        }
    }
}

/// Comment joindre l'éditeur par UDP.
#[derive(Debug, Clone)]
pub enum Discovery {
    /// Multicast comme le client de référence (lignes 258-268 de remote_execution.py).
    Multicast {
        group: Ipv4Addr,
        port: u16,
        bind_address: Ipv4Addr,
        ttl: u32,
    },
    /// Datagrammes directs (faux nœud de test): on se lie à `bind`, on écrit à `peer`.
    Direct { bind: SocketAddr, peer: SocketAddr },
}

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub discovery: Discovery,
    /// Interface sur laquelle l'éditeur doit se connecter en TCP (et qu'on lui annonce).
    pub command_address: Ipv4Addr,
    /// Projet à sélectionner (champ `project_name` du pong). `None`: premier nœud trouvé.
    pub project_name: Option<String>,
    pub discovery_timeout: Duration,
    pub connect_timeout: Duration,
    pub command_timeout: Duration,
}

impl BridgeConfig {
    /// Valeurs du client de référence: 239.0.0.1:6766, interface 127.0.0.1, TTL 0.
    pub fn default_multicast() -> BridgeConfig {
        BridgeConfig {
            discovery: Discovery::Multicast {
                group: Ipv4Addr::new(239, 0, 0, 1),
                port: 6766,
                bind_address: Ipv4Addr::LOCALHOST,
                ttl: 0,
            },
            command_address: Ipv4Addr::LOCALHOST,
            project_name: Some(DEFAULT_PROJECT_NAME.to_owned()),
            discovery_timeout: Duration::from_secs(3),
            connect_timeout: Duration::from_secs(5),
            command_timeout: Duration::from_secs(15),
        }
    }

    /// `UE_PROJECT_NAME` règle le projet cherché (vide: n'importe quel éditeur).
    pub fn from_vars(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> BridgeConfig {
        let mut cfg = BridgeConfig::default_multicast();
        if let Some(name) = get("UE_PROJECT_NAME") {
            let name = name.to_string_lossy().trim().to_owned();
            cfg.project_name = (!name.is_empty()).then_some(name);
        }
        cfg
    }
}

/// Un éditeur découvert.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    /// Charge utile du `pong`, telle que reçue.
    pub data: Value,
}

impl Node {
    pub fn project_name(&self) -> Option<&str> {
        self.data.get("project_name").and_then(Value::as_str)
    }
}

/// Résultat brut d'une commande.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandOutput {
    pub node: Node,
    pub success: bool,
    /// Champ `result` du `command_result` (contient le traceback en cas d'échec).
    pub result: String,
    /// Lignes imprimées par le script (champ `output`), jointes par des sauts de ligne.
    pub output: String,
}

struct Udp {
    socket: UdpSocket,
    send_to: SocketAddr,
}

impl Udp {
    fn open(cfg: &BridgeConfig) -> Result<Udp, UeError> {
        let sock_err = |what: &str, e: std::io::Error| UeError::Socket(format!("{what}: {e}"));
        match &cfg.discovery {
            Discovery::Multicast {
                group,
                port,
                bind_address,
                ttl,
            } => {
                let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
                    .map_err(|e| sock_err("création du socket UDP", e))?;
                // L'éditeur écoute sur le même port: adresse réutilisable (ligne 259-262).
                socket
                    .set_reuse_address(true)
                    .map_err(|e| sock_err("SO_REUSEADDR", e))?;
                socket
                    .bind(&SockAddr::from(SocketAddrV4::new(*bind_address, *port)))
                    .map_err(|e| sock_err(&format!("liaison à {bind_address}:{port}"), e))?;
                socket
                    .set_multicast_loop_v4(true)
                    .map_err(|e| sock_err("IP_MULTICAST_LOOP", e))?;
                socket
                    .set_multicast_ttl_v4(*ttl)
                    .map_err(|e| sock_err("IP_MULTICAST_TTL", e))?;
                socket
                    .set_multicast_if_v4(bind_address)
                    .map_err(|e| sock_err("IP_MULTICAST_IF", e))?;
                socket
                    .join_multicast_v4(group, bind_address)
                    .map_err(|e| sock_err(&format!("adhésion au groupe {group}"), e))?;
                let socket: UdpSocket = socket.into();
                socket
                    .set_read_timeout(Some(UDP_POLL))
                    .map_err(|e| sock_err("délai de lecture UDP", e))?;
                Ok(Udp {
                    socket,
                    send_to: SocketAddr::V4(SocketAddrV4::new(*group, *port)),
                })
            }
            Discovery::Direct { bind, peer } => {
                let socket = UdpSocket::bind(bind).map_err(|e| sock_err("liaison UDP", e))?;
                socket
                    .set_read_timeout(Some(UDP_POLL))
                    .map_err(|e| sock_err("délai de lecture UDP", e))?;
                Ok(Udp {
                    socket,
                    send_to: *peer,
                })
            }
        }
    }

    fn send(&self, message: &Message) -> Result<(), UeError> {
        self.socket
            .send_to(&message.to_bytes(), self.send_to)
            .map(|_| ())
            .map_err(|e| UeError::Socket(format!("envoi UDP vers {}: {e}", self.send_to)))
    }

    /// Un datagramme (ou `None` au bout du délai de lecture).
    fn receive(&self) -> Result<Option<Vec<u8>>, UeError> {
        let mut buf = vec![0u8; 65_535];
        match self.socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                buf.truncate(n);
                Ok(Some(buf))
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(UeError::Socket(format!("lecture UDP: {e}"))),
        }
    }
}

/// Découvre les éditeurs pendant `discovery_timeout` et renvoie le premier qui correspond au
/// projet demandé. Les nœuds vus sont décrits dans l'erreur si aucun ne correspond.
fn find_node(cfg: &BridgeConfig, udp: &Udp, node_id: &str) -> Result<Node, UeError> {
    let start = Instant::now();
    let mut last_ping: Option<Instant> = None;
    let mut seen: BTreeMap<String, Node> = BTreeMap::new();
    while start.elapsed() < cfg.discovery_timeout {
        if last_ping.is_none_or(|t| t.elapsed() >= PING_INTERVAL) {
            udp.send(&Message::new(TYPE_PING, node_id, None, None))?;
            last_ping = Some(Instant::now());
        }
        let Some(bytes) = udp.receive()? else {
            continue;
        };
        let Ok(msg) = Message::from_bytes(&bytes) else {
            continue; // version, magic ou format incorrect: ignoré comme le client de référence
        };
        if !msg.passes_receive_filter(node_id) || msg.kind != TYPE_PONG {
            continue;
        }
        let node = Node {
            id: msg.source.clone(),
            data: msg.data.unwrap_or_else(|| json!({})),
        };
        let matches = match (&cfg.project_name, node.project_name()) {
            (None, _) => true,
            (Some(want), Some(got)) => want.eq_ignore_ascii_case(got),
            (Some(_), None) => false,
        };
        if matches {
            return Ok(node);
        }
        seen.insert(msg.source, node);
    }
    let waited = cfg.discovery_timeout.as_secs_f64();
    if seen.is_empty() {
        return Err(UeError::DevkitNotRunning(format!(
            "aucun éditeur Unreal n'a répondu en {waited:.0} s ({}). Le Devkit doit être ouvert (le \
             démarrage prend environ 2 minutes) avec Python Remote Execution activé.",
            describe_discovery(&cfg.discovery)
        )));
    }
    let projects: Vec<String> = seen
        .values()
        .map(|n| match n.project_name() {
            Some(p) => format!("{p:?}"),
            None => format!("(sans project_name) {}", n.data),
        })
        .collect();
    Err(UeError::DevkitNotRunning(format!(
        "{} éditeur(s) ont répondu, aucun pour le projet {:?}: {}. Régler UE_PROJECT_NAME si le projet \
         s'appelle autrement.",
        seen.len(),
        cfg.project_name.as_deref().unwrap_or("*"),
        projects.join(", ")
    )))
}

fn describe_discovery(d: &Discovery) -> String {
    match d {
        Discovery::Multicast {
            group,
            port,
            bind_address,
            ..
        } => format!("multicast {group}:{port} via {bind_address}"),
        Discovery::Direct { peer, .. } => format!("datagrammes vers {peer}"),
    }
}

/// Ouvre le canal TCP et exécute une commande. `open_connection` est renvoyé chaque seconde
/// jusqu'à la connexion de l'éditeur.
fn execute(
    cfg: &BridgeConfig,
    udp: &Udp,
    node_id: &str,
    node: &Node,
    script: &str,
) -> Result<CommandOutput, UeError> {
    let sock_err = |what: &str, e: std::io::Error| UeError::Socket(format!("{what}: {e}"));
    let listener = TcpListener::bind(SocketAddrV4::new(cfg.command_address, 0))
        .map_err(|e| sock_err("serveur TCP éphémère", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| sock_err("adresse du serveur TCP", e))?
        .port();
    listener
        .set_nonblocking(true)
        .map_err(|e| sock_err("accept non bloquant", e))?;
    let open = Message::new(
        TYPE_OPEN_CONNECTION,
        node_id,
        Some(&node.id),
        Some(json!({"command_ip": cfg.command_address.to_string(), "command_port": port})),
    );

    let started = Instant::now();
    let mut last_open: Option<Instant> = None;
    let mut stream = loop {
        if last_open.is_none_or(|t| t.elapsed() >= PING_INTERVAL) {
            udp.send(&open)?;
            last_open = Some(Instant::now());
        }
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if started.elapsed() >= cfg.connect_timeout {
                    return Err(UeError::Timeout {
                        stage: "connexion de l'éditeur au serveur TCP local",
                        secs: cfg.connect_timeout.as_secs_f64(),
                    });
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(sock_err("accept", e)),
        }
    };
    stream
        .set_nonblocking(false)
        .map_err(|e| sock_err("mode bloquant", e))?;

    let command = Message::new(
        TYPE_COMMAND,
        node_id,
        Some(&node.id),
        Some(json!({"command": script, "unattended": true, "exec_mode": MODE_EXEC_FILE})),
    );
    stream
        .set_write_timeout(Some(cfg.command_timeout))
        .map_err(|e| sock_err("délai d'écriture", e))?;
    stream
        .write_all(&command.to_bytes())
        .map_err(|e| UeError::ProtocolError(format!("envoi de la commande impossible: {e}")))?;

    let reply = read_json_message(&mut stream, cfg.command_timeout)?;
    let message = Message::from_value(&reply).map_err(|e| UeError::ProtocolError(e.to_string()))?;
    if !message.passes_receive_filter(node_id) {
        return Err(UeError::ProtocolError(
            "la réponse n'est pas adressée à cette session".into(),
        ));
    }
    if message.kind != TYPE_COMMAND_RESULT {
        return Err(UeError::ProtocolError(format!(
            "type de message inattendu {:?} (attendu {TYPE_COMMAND_RESULT:?})",
            message.kind
        )));
    }
    let data = message
        .data
        .ok_or_else(|| UeError::ProtocolError("command_result sans data".into()))?;
    let success = data
        .get("success")
        .and_then(Value::as_bool)
        .ok_or_else(|| UeError::ProtocolError("command_result sans champ success".into()))?;
    Ok(CommandOutput {
        node: node.clone(),
        success,
        result: data
            .get("result")
            .map(|r| r.as_str().map_or_else(|| r.to_string(), str::to_owned))
            .unwrap_or_default(),
        output: output_text(data.get("output")),
    })
}

/// Le champ `output` est une liste d'entrées `{type, output}` (ou, par tolérance, une chaîne).
fn output_text(output: Option<&Value>) -> String {
    match output {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| match i {
                Value::String(s) => Some(s.as_str()),
                other => other.get("output").and_then(Value::as_str),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Lit un objet JSON complet sur le flux: aucun délimiteur n'existe (docs §5), on accumule jusqu'à
/// ce que le JSON soit complet. `timeout` borne l'ensemble de la lecture.
fn read_json_message(stream: &mut TcpStream, timeout: Duration) -> Result<Value, UeError> {
    let deadline = Instant::now() + timeout;
    let mut data: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(UeError::Timeout {
                stage: STAGE_COMMAND,
                secs: timeout.as_secs_f64(),
            });
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|e| UeError::Socket(format!("délai de lecture: {e}")))?;
        match stream.read(&mut chunk) {
            Ok(0) => {
                return Err(UeError::ProtocolError(if data.is_empty() {
                    "l'éditeur a fermé la connexion sans répondre".into()
                } else {
                    format!(
                        "l'éditeur a fermé la connexion au milieu d'un message ({} octets reçus)",
                        data.len()
                    )
                }))
            }
            Ok(n) => data.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(UeError::Timeout {
                    stage: STAGE_COMMAND,
                    secs: timeout.as_secs_f64(),
                })
            }
            Err(e) => return Err(UeError::ProtocolError(format!("lecture TCP: {e}"))),
        }
        if data.len() > MAX_REPLY_BYTES {
            return Err(UeError::ProtocolError("réponse trop volumineuse".into()));
        }
        match serde_json::from_slice::<Value>(&data) {
            Ok(v) => return Ok(v),
            Err(e) if e.is_eof() => continue, // message incomplet: on attend la suite
            Err(e) => {
                return Err(UeError::ProtocolError(format!(
                    "réponse JSON invalide: {e}"
                )))
            }
        }
    }
}

/// Liste les éditeurs qui répondent pendant `discovery_timeout` (diagnostic).
pub fn discover_nodes(cfg: &BridgeConfig) -> Result<Vec<Node>, UeError> {
    let node_id = uuid::Uuid::new_v4().to_string();
    let udp = Udp::open(cfg)?;
    let start = Instant::now();
    let mut last_ping: Option<Instant> = None;
    let mut nodes: BTreeMap<String, Node> = BTreeMap::new();
    while start.elapsed() < cfg.discovery_timeout {
        if last_ping.is_none_or(|t| t.elapsed() >= PING_INTERVAL) {
            udp.send(&Message::new(TYPE_PING, &node_id, None, None))?;
            last_ping = Some(Instant::now());
        }
        let Some(bytes) = udp.receive()? else {
            continue;
        };
        if let Ok(msg) = Message::from_bytes(&bytes) {
            if msg.passes_receive_filter(&node_id) && msg.kind == TYPE_PONG {
                nodes.insert(
                    msg.source.clone(),
                    Node {
                        id: msg.source,
                        data: msg.data.unwrap_or_else(|| json!({})),
                    },
                );
            }
        }
    }
    Ok(nodes.into_values().collect())
}

/// Une session complète: découverte, connexion, exécution de `script`, fermeture. Les sockets
/// sont fermés dans tous les cas, `close_connection` est envoyé dès qu'une ouverture a été tentée.
pub fn run_script(cfg: &BridgeConfig, script: &str) -> Result<CommandOutput, UeError> {
    let node_id = uuid::Uuid::new_v4().to_string();
    let udp = Udp::open(cfg)?;
    let node = find_node(cfg, &udp, &node_id)?;
    let result = execute(cfg, &udp, &node_id, &node, script);
    let _ = udp.send(&Message::new(
        TYPE_CLOSE_CONNECTION,
        &node_id,
        Some(&node.id),
        None,
    ));
    result
}

/// Exécute un gabarit figé avec ses paramètres et renvoie la charge JSON imprimée entre marqueurs.
pub fn run_template(
    cfg: &BridgeConfig,
    template: &templates::Template,
    params: &Value,
) -> Result<TemplateOutput, UeError> {
    let script = template.render(params);
    let out = run_script(cfg, &script)?;
    if !out.success {
        return Err(UeError::PythonError(if out.result.trim().is_empty() {
            out.output.clone()
        } else {
            out.result.clone()
        }));
    }
    // Les marqueurs sont dans la sortie imprimée; par tolérance, on cherche aussi dans `result`.
    let payload = templates::extract_payload(&out.output)
        .or_else(|_| templates::extract_payload(&out.result))
        .map_err(UeError::ProtocolError)?;
    if let Some(err) = payload.get("error").and_then(Value::as_str) {
        return Err(UeError::PythonError(err.to_owned()));
    }
    Ok(TemplateOutput {
        node: out.node,
        payload,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct TemplateOutput {
    pub node: Node,
    pub payload: Value,
}
