#![allow(dead_code)]
//! Banc d'essai client↔serveur en mémoire (aucun processus, aucun port réseau).

use std::path::{Path, PathBuf};

use conan_core::Config;
use mcp_server::build_server;
use rmcp::{
    model::{CallToolRequestParams, CallToolResult, ClientConfig},
    service::RunningService,
    RoleClient, ServiceExt,
};
use serde_json::{json, Value};

#[derive(Debug, Clone, Default)]
pub struct TestClient;

impl rmcp::ClientHandler for TestClient {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::default()
    }
}

pub struct Session {
    pub client: RunningService<RoleClient, TestClient>,
}

/// Démarre un serveur configuré par `config` et y connecte un client en mémoire.
pub async fn connect(config: &Config) -> Session {
    let server = build_server(config).expect("serveur");
    let (client_write, server_read) = tokio::io::duplex(1 << 20);
    let (server_write, client_read) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        let running = server
            .serve((server_read, server_write))
            .await
            .expect("serve");
        let _ = running.waiting().await;
    });
    let client = TestClient
        .serve((client_read, client_write))
        .await
        .expect("client");
    Session { client }
}

impl Session {
    pub async fn call(&self, tool: &str, args: Value) -> CallToolResult {
        let map = args.as_object().expect("arguments objet").clone();
        self.client
            .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(map))
            .await
            .expect("appel d'outil (erreur de protocole)")
    }
}

pub fn text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn is_error(r: &CallToolResult) -> bool {
    r.is_error == Some(true)
}

pub fn structured(r: &CallToolResult) -> &Value {
    r.structured_content.as_ref().expect("contenu structuré")
}

/// Table réelle (export Devkit) et son schéma.
pub const TABLE: &str = "DT_Purge_Nordheimer";
pub const CSV_V1: &str = include_str!("../../../../fixtures/DT_Purge_Nordheimer.csv");
pub const SCHEMA_TOML: &str = include_str!("../../../../schemas/DT_Purge_Nordheimer.toml");

/// V1 + une ligne ajoutée + un poids modifié.
pub fn csv_v2() -> String {
    let mut v = CSV_V1.replace("\"10.000000\"", "\"12.000000\"");
    v.push_str("Trash5,\"Nordheimer\",\"PurgeNew_Nordheimer_Grunt_3\",\"2.000000\",\"WeightedSpawnTable\"\n");
    v
}

/// `<tmp>/mod` (modinfo.json, active.txt, Content/Data/m.csv), `<tmp>/data/csv` et
/// `<tmp>/schemas` (avec le schéma de la table réelle).
pub struct Env {
    pub tmp: tempfile::TempDir,
    pub mod_root: PathBuf,
    pub csv_root: PathBuf,
    pub schema_root: PathBuf,
}

impl Env {
    pub fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let mod_root = tmp.path().join("mod");
        std::fs::create_dir_all(mod_root.join("Content/Data")).unwrap();
        std::fs::write(mod_root.join("modinfo.json"), "{\"name\":\"test\"}").unwrap();
        std::fs::write(mod_root.join("active.txt"), "").unwrap();
        std::fs::write(mod_root.join("Content/Data/m.csv"), CSV_V1).unwrap();
        let schema_root = tmp.path().join("schemas");
        std::fs::create_dir_all(&schema_root).unwrap();
        std::fs::write(schema_root.join(format!("{TABLE}.toml")), SCHEMA_TOML).unwrap();
        let csv_root = tmp.path().join("data").join("csv");
        Env {
            tmp,
            mod_root,
            csv_root,
            schema_root,
        }
    }

    pub fn config(&self) -> Config {
        Config::new(&self.mod_root, Some(self.csv_root.clone())).with_schema_root(&self.schema_root)
    }

    /// Même configuration, mais SCHEMA_ROOT non configuré.
    pub fn config_without_schemas(&self) -> Config {
        Config::new(&self.mod_root, Some(self.csv_root.clone()))
    }
}

/// Contenu (chemin relatif, octets) de tous les fichiers sous `dir`.
pub fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push((
                    p.strip_prefix(dir).unwrap().to_path_buf(),
                    std::fs::read(&p).unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
}

pub fn write_args(
    table: &str,
    content: &str,
    dry_run: Option<bool>,
    confirm: Option<bool>,
) -> Value {
    let mut v = json!({"table": table, "content": content});
    if let Some(d) = dry_run {
        v["dry_run"] = json!(d);
    }
    if let Some(c) = confirm {
        v["confirm"] = json!(c);
    }
    v
}
