//! Faux éditeur Unreal pour les tests: répond au protocole (UDP + TCP) sur la machine locale,
//! sans Devkit. Sert aux tests du pont et des outils MCP; n'est jamais utilisé par le serveur.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};

use super::message::{
    Message, TYPE_CLOSE_CONNECTION, TYPE_COMMAND, TYPE_COMMAND_RESULT, TYPE_OPEN_CONNECTION,
    TYPE_PING, TYPE_PONG,
};
use super::{BridgeConfig, Discovery};

/// Ce que le faux éditeur répond à une commande.
#[derive(Debug, Clone)]
pub enum Reply {
    /// `command_result` de succès: `result` + une entrée de sortie par ligne.
    Ok { result: String, output: Vec<String> },
    /// `command_result` d'échec avec un traceback dans `result`.
    PythonError { traceback: String },
    /// Aucune réponse (la connexion reste ouverte).
    Silence,
    /// Octets qui ne sont pas du JSON.
    Garbage,
    /// Un message valide mais du mauvais type.
    WrongType,
    /// Ferme la connexion sans répondre.
    Hangup,
    /// Réponse de succès envoyée en petits morceaux espacés (le découpage TCP n'a pas de cadre).
    Chunked { output: Vec<String> },
}

impl Reply {
    pub fn printing(text: &str) -> Reply {
        Reply::Ok {
            result: "None".into(),
            output: vec![text.to_owned()],
        }
    }
}

type Handler = dyn Fn(&str) -> Reply + Send + Sync;

pub struct FakeNode {
    addr: SocketAddr,
    node_id: String,
    stop: Arc<AtomicBool>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// Commandes Python reçues (texte du champ `command`).
    pub commands: Arc<Mutex<Vec<String>>>,
    /// Données complètes des `command` reçues.
    pub command_data: Arc<Mutex<Vec<Value>>>,
    pub opens: Arc<AtomicUsize>,
    pub closes: Arc<AtomicUsize>,
    pub pings: Arc<AtomicUsize>,
}

#[derive(Clone)]
pub struct Behavior {
    /// Champ `project_name` du pong (`None`: pong sans ce champ).
    pub project_name: Option<String>,
    /// Répondre aux pings.
    pub answer_pings: bool,
    /// Se connecter au serveur TCP après `open_connection`.
    pub connect_back: bool,
    /// Version annoncée dans les pongs (1 = correcte).
    pub pong_version: u64,
}

impl Default for Behavior {
    fn default() -> Self {
        Behavior {
            project_name: Some(super::DEFAULT_PROJECT_NAME.to_owned()),
            answer_pings: true,
            connect_back: true,
            pong_version: 1,
        }
    }
}

impl FakeNode {
    pub fn start(
        behavior: Behavior,
        handler: impl Fn(&str) -> Reply + Send + Sync + 'static,
    ) -> FakeNode {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("udp du faux éditeur");
        socket
            .set_read_timeout(Some(Duration::from_millis(30)))
            .expect("délai UDP");
        let addr = socket.local_addr().expect("adresse du faux éditeur");
        let node_id = uuid::Uuid::new_v4().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let command_data = Arc::new(Mutex::new(Vec::new()));
        let (opens, closes, pings) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let handler: Arc<Handler> = Arc::new(handler);
        let node = FakeNode {
            addr,
            node_id: node_id.clone(),
            stop: stop.clone(),
            threads: Mutex::new(Vec::new()),
            commands: commands.clone(),
            command_data: command_data.clone(),
            opens: opens.clone(),
            closes: closes.clone(),
            pings: pings.clone(),
        };
        let main = {
            let (stop, commands, command_data) = (stop.clone(), commands, command_data);
            let (opens, closes, pings) = (opens, closes, pings);
            let node_id = node_id.clone();
            let behavior = behavior.clone();
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 65_535];
                let mut workers: Vec<JoinHandle<()>> = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    let Ok((n, from)) = socket.recv_from(&mut buf) else {
                        continue;
                    };
                    let Ok(msg) = Message::from_bytes(&buf[..n]) else {
                        continue;
                    };
                    if !msg.passes_receive_filter(&node_id) {
                        continue;
                    }
                    match msg.kind.as_str() {
                        TYPE_PING => {
                            pings.fetch_add(1, Ordering::SeqCst);
                            if behavior.answer_pings {
                                let mut data = json!({"machine": "fake", "user": "fake", "engine_version": "5.8.2"});
                                if let Some(p) = &behavior.project_name {
                                    data["project_name"] = json!(p);
                                }
                                let mut bytes =
                                    Message::new(TYPE_PONG, &node_id, None, Some(data)).to_bytes();
                                if behavior.pong_version != 1 {
                                    if let Ok(mut v) = serde_json::from_slice::<Value>(&bytes) {
                                        v["version"] = json!(behavior.pong_version);
                                        bytes = serde_json::to_vec(&v).unwrap_or_default();
                                    }
                                }
                                let _ = socket.send_to(&bytes, from);
                            }
                        }
                        TYPE_OPEN_CONNECTION => {
                            opens.fetch_add(1, Ordering::SeqCst);
                            if !behavior.connect_back {
                                continue;
                            }
                            let data = msg.data.unwrap_or_default();
                            let (Some(ip), Some(port)) = (
                                data.get("command_ip").and_then(Value::as_str),
                                data.get("command_port").and_then(Value::as_u64),
                            ) else {
                                continue;
                            };
                            let Ok(stream) = TcpStream::connect(format!("{ip}:{port}")) else {
                                continue;
                            };
                            let (handler, commands, command_data, node_id, source, stop) = (
                                handler.clone(),
                                commands.clone(),
                                command_data.clone(),
                                node_id.clone(),
                                msg.source.clone(),
                                stop.clone(),
                            );
                            workers.push(std::thread::spawn(move || {
                                serve_connection(
                                    stream,
                                    &handler,
                                    &commands,
                                    &command_data,
                                    &node_id,
                                    &source,
                                    &stop,
                                );
                            }));
                        }
                        TYPE_CLOSE_CONNECTION => {
                            closes.fetch_add(1, Ordering::SeqCst);
                        }
                        _ => {}
                    }
                }
                for w in workers {
                    let _ = w.join();
                }
            })
        };
        node.threads.lock().map(|mut t| t.push(main)).ok();
        node
    }

    /// Configuration de pont qui vise ce faux éditeur (délais courts).
    pub fn config(&self) -> BridgeConfig {
        BridgeConfig {
            discovery: Discovery::Direct {
                bind: "127.0.0.1:0".parse().expect("adresse"),
                peer: self.addr,
            },
            command_address: std::net::Ipv4Addr::LOCALHOST,
            project_name: Some(super::DEFAULT_PROJECT_NAME.to_owned()),
            discovery_timeout: Duration::from_millis(800),
            connect_timeout: Duration::from_millis(800),
            command_timeout: Duration::from_millis(1500),
        }
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Adresse qui n'écoute personne (éditeur absent).
    pub fn absent_config() -> BridgeConfig {
        let silent = UdpSocket::bind("127.0.0.1:0").expect("socket muet");
        let peer = silent.local_addr().expect("adresse");
        // `silent` reste ouvert le temps du test grâce à la fuite volontaire: personne ne lit.
        std::mem::forget(silent);
        BridgeConfig {
            discovery: Discovery::Direct {
                bind: "127.0.0.1:0".parse().expect("adresse"),
                peer,
            },
            command_address: std::net::Ipv4Addr::LOCALHOST,
            project_name: Some(super::DEFAULT_PROJECT_NAME.to_owned()),
            discovery_timeout: Duration::from_millis(400),
            connect_timeout: Duration::from_millis(400),
            command_timeout: Duration::from_millis(800),
        }
    }
}

impl Drop for FakeNode {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(mut threads) = self.threads.lock() {
            for t in threads.drain(..) {
                let _ = t.join();
            }
        }
    }
}

fn serve_connection(
    mut stream: TcpStream,
    handler: &Arc<Handler>,
    commands: &Arc<Mutex<Vec<String>>>,
    command_data: &Arc<Mutex<Vec<Value>>>,
    node_id: &str,
    client_id: &str,
    stop: &Arc<AtomicBool>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let mut data = Vec::new();
    let mut chunk = [0u8; 8192];
    // une commande, puis on garde la connexion jusqu'à fermeture par le client
    let request = loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => data.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(_) => return,
        }
        if let Ok(v) = serde_json::from_slice::<Value>(&data) {
            break v;
        }
    };
    let Ok(msg) = Message::from_value(&request) else {
        return;
    };
    if msg.kind != TYPE_COMMAND {
        return;
    }
    let payload = msg.data.unwrap_or_default();
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if let Ok(mut c) = commands.lock() {
        c.push(command.clone());
    }
    if let Ok(mut c) = command_data.lock() {
        c.push(payload);
    }
    let result_message =
        |data: Value| Message::new(TYPE_COMMAND_RESULT, node_id, Some(client_id), Some(data));
    match handler(&command) {
        Reply::Ok { result, output } => {
            let out: Vec<Value> = output
                .iter()
                .map(|l| json!({"type": "Info", "output": l}))
                .collect();
            let _ = stream.write_all(
                &result_message(json!({"success": true, "result": result, "output": out}))
                    .to_bytes(),
            );
        }
        Reply::PythonError { traceback } => {
            let _ = stream.write_all(
                &result_message(json!({"success": false, "result": traceback, "output": []}))
                    .to_bytes(),
            );
        }
        Reply::Garbage => {
            let _ = stream.write_all(b"ceci n'est pas du json{{");
        }
        Reply::WrongType => {
            let _ = stream.write_all(
                &Message::new(TYPE_PONG, node_id, Some(client_id), Some(json!({}))).to_bytes(),
            );
        }
        Reply::Hangup => return,
        Reply::Silence => {}
        Reply::Chunked { output } => {
            let out: Vec<Value> = output
                .iter()
                .map(|l| json!({"type": "Info", "output": l}))
                .collect();
            let bytes = result_message(json!({"success": true, "result": "None", "output": out}))
                .to_bytes();
            for piece in bytes.chunks(97) {
                if stream.write_all(piece).is_err() {
                    return;
                }
                let _ = stream.flush();
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
    // garde la connexion ouverte jusqu'à ce que le client la ferme (ou arrêt)
    let mut sink = [0u8; 64];
    while !stop.load(Ordering::SeqCst) {
        match stream.read(&mut sink) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ue_bridge::{discover_nodes, run_script, run_template, templates, UeError};

    fn ok_node(handler: impl Fn(&str) -> Reply + Send + Sync + 'static) -> FakeNode {
        FakeNode::start(Behavior::default(), handler)
    }

    #[test]
    fn full_session_success_sends_the_command_and_closes() {
        let node = ok_node(|cmd| Reply::printing(&format!("echo:{}", cmd.len())));
        let out = run_script(&node.config(), "print('bonjour é')").unwrap();
        assert!(out.success);
        assert_eq!(out.output, "echo:19");
        assert_eq!(out.node.id, node.node_id());
        assert_eq!(out.node.project_name(), Some("ConanSandbox"));
        // la commande est reçue telle quelle, en mode ExecuteFile, non interactive
        assert_eq!(
            node.commands.lock().unwrap().as_slice(),
            ["print('bonjour é')"]
        );
        let data = node.command_data.lock().unwrap()[0].clone();
        assert_eq!(data["exec_mode"], "ExecuteFile");
        assert_eq!(data["unattended"], true);
        // close_connection est envoyé; une seule ouverture
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(node.closes.load(Ordering::SeqCst), 1);
        assert_eq!(node.opens.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn devkit_not_running_when_nobody_answers() {
        let started = std::time::Instant::now();
        let err = run_script(&FakeNode::absent_config(), "pass").unwrap_err();
        assert!(matches!(err, UeError::DevkitNotRunning(_)), "{err}");
        assert_eq!(err.code(), "DevkitNotRunning");
        assert!(err.to_string().contains("Devkit") && err.to_string().contains("ouvert"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn wrong_project_is_reported_with_the_projects_seen() {
        let node = FakeNode::start(
            Behavior {
                project_name: Some("AutreProjet".into()),
                ..Behavior::default()
            },
            |_| Reply::printing("x"),
        );
        let err = run_script(&node.config(), "pass").unwrap_err();
        assert!(
            matches!(&err, UeError::DevkitNotRunning(m) if m.contains("AutreProjet") && m.contains("UE_PROJECT_NAME")),
            "{err}"
        );
        // sans filtre de projet, il est accepté
        let mut cfg = node.config();
        cfg.project_name = None;
        assert!(run_script(&cfg, "pass").is_ok());
        // la casse du nom de projet est ignorée
        let mut cfg = node.config();
        cfg.project_name = Some("autreprojet".into());
        assert!(run_script(&cfg, "pass").is_ok());
        // un pong sans project_name ne correspond à aucun filtre
        let anonymous = FakeNode::start(
            Behavior {
                project_name: None,
                ..Behavior::default()
            },
            |_| Reply::printing("x"),
        );
        assert!(matches!(
            run_script(&anonymous.config(), "pass").unwrap_err(),
            UeError::DevkitNotRunning(_)
        ));
    }

    #[test]
    fn pongs_with_a_wrong_protocol_version_are_ignored() {
        let node = FakeNode::start(
            Behavior {
                pong_version: 2,
                ..Behavior::default()
            },
            |_| Reply::printing("x"),
        );
        assert!(matches!(
            run_script(&node.config(), "pass").unwrap_err(),
            UeError::DevkitNotRunning(_)
        ));
    }

    #[test]
    fn timeout_when_the_editor_never_connects() {
        let node = FakeNode::start(
            Behavior {
                connect_back: false,
                ..Behavior::default()
            },
            |_| Reply::printing("x"),
        );
        let err = run_script(&node.config(), "pass").unwrap_err();
        assert!(
            matches!(err, UeError::Timeout { stage, .. } if stage.contains("connexion")),
            "{err}"
        );
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            node.closes.load(Ordering::SeqCst),
            1,
            "close_connection est quand même envoyé"
        );
    }

    #[test]
    fn timeout_when_the_command_never_answers_and_the_session_is_closed() {
        let node = ok_node(|_| Reply::Silence);
        let mut cfg = node.config();
        cfg.command_timeout = Duration::from_millis(300);
        let started = std::time::Instant::now();
        let err = run_script(&cfg, "while True: pass").unwrap_err();
        assert!(
            matches!(err, UeError::Timeout { stage, .. } if stage.contains("commande")),
            "{err}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(node.closes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn python_traceback_becomes_a_python_error() {
        let tb = "Traceback (most recent call last):\n  File \"<string>\", line 3, in <module>\nNameError: name 'x' is not defined";
        let node = ok_node(move |_| Reply::PythonError {
            traceback: tb.into(),
        });
        let out = run_script(&node.config(), "x").unwrap();
        assert!(!out.success && out.result.contains("NameError"));
        let err = run_template(&node.config(), &templates::STATUS, &json!({})).unwrap_err();
        assert!(
            matches!(&err, UeError::PythonError(t) if t.contains("NameError") && t.contains("Traceback")),
            "{err}"
        );
        assert_eq!(err.code(), "PythonError");
    }

    #[test]
    fn protocol_errors_garbage_wrong_type_and_hangup() {
        for reply in [Reply::Garbage, Reply::WrongType, Reply::Hangup] {
            let node = ok_node(move |_| reply.clone());
            let err = run_script(&node.config(), "pass").unwrap_err();
            assert!(matches!(err, UeError::ProtocolError(_)), "{err}");
            std::thread::sleep(Duration::from_millis(100));
            assert_eq!(node.closes.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn replies_split_in_many_tcp_segments_are_reassembled() {
        let long = "x".repeat(30_000);
        let long2 = long.clone();
        let node = ok_node(move |_| Reply::Chunked {
            output: vec![long2.clone(), "fin".into()],
        });
        let mut cfg = node.config();
        cfg.command_timeout = Duration::from_secs(5);
        let out = run_script(&cfg, "pass").unwrap();
        assert_eq!(out.output, format!("{long}\nfin"));
    }

    #[test]
    fn template_roundtrip_through_the_markers() {
        let node = ok_node(|cmd| {
            // le faux éditeur vérifie que les paramètres sont bien dans un littéral json.loads
            assert!(cmd.contains("json.loads(\""));
            Reply::printing(&format!(
                "LogPython: {}{{\"total\": 3}}{}",
                templates::MARKER_BEGIN,
                templates::MARKER_END
            ))
        });
        let out = run_template(
            &node.config(),
            &templates::SEARCH_ASSETS,
            &json!({"limit": 3}),
        )
        .unwrap();
        assert_eq!(out.payload["total"], 3);
        // erreur signalée par le script lui-même
        let node = ok_node(|_| {
            Reply::printing(&format!(
                "{}{{\"error\": \"introuvable\"}}{}",
                templates::MARKER_BEGIN,
                templates::MARKER_END
            ))
        });
        assert!(
            matches!(run_template(&node.config(), &templates::DESCRIBE_ASSET, &json!({})).unwrap_err(), UeError::PythonError(m) if m == "introuvable")
        );
        // sortie sans marqueurs
        let node = ok_node(|_| Reply::printing("rien d'utile"));
        assert!(matches!(
            run_template(&node.config(), &templates::STATUS, &json!({})).unwrap_err(),
            UeError::ProtocolError(_)
        ));
    }

    #[test]
    fn discovery_lists_every_answering_editor() {
        let a = ok_node(|_| Reply::printing("x"));
        let nodes = discover_nodes(&a.config()).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, a.node_id());
        assert_eq!(nodes[0].data["engine_version"], "5.8.2");
        assert!(a.pings.load(Ordering::SeqCst) >= 1);
        assert!(discover_nodes(&FakeNode::absent_config())
            .unwrap()
            .is_empty());
    }
}
