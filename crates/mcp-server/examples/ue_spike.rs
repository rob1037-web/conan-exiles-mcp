//! Outil de mise au point (lecture seule): exécute un gabarit figé contre le Devkit ouvert.
//!
//!     ue_spike discover
//!     ue_spike <status|search_assets|describe_asset|blueprint_variables> '<paramètres JSON>'
//!
//! N'exécute AUCUN script libre: seuls les gabarits de `ue_bridge::templates` sont acceptés.
//! `UE_PROJECT_NAME` règle le projet cherché (défaut ConanSandbox).

use std::process::ExitCode;

use mcp_server::ue_bridge::{discover_nodes, run_template, templates, BridgeConfig};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = BridgeConfig::from_vars(|n| std::env::var_os(n));
    match args.first().map(String::as_str) {
        Some("discover") => match discover_nodes(&cfg) {
            Ok(nodes) => {
                println!("{} éditeur(s):", nodes.len());
                for n in nodes {
                    println!("  node_id={} pong={}", n.id, n.data);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                println!("ERREUR {}: {e}", e.code());
                ExitCode::FAILURE
            }
        },
        Some(name) => {
            let Some(template) = templates::ALL.iter().find(|t| t.name == name) else {
                eprintln!("gabarit inconnu: {name}");
                return ExitCode::from(2);
            };
            let params: serde_json::Value = args
                .get(1)
                .map(|p| serde_json::from_str(p))
                .transpose()
                .unwrap_or_else(|e| {
                    eprintln!("paramètres JSON invalides: {e}");
                    std::process::exit(2)
                })
                .unwrap_or_else(|| serde_json::json!({}));
            match run_template(&cfg, template, &params) {
                Ok(out) => {
                    println!("noeud: {} ({})", out.node.id, out.node.data);
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&out.payload).unwrap_or_default()
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    println!("ERREUR {}: {e}", e.code());
                    ExitCode::FAILURE
                }
            }
        }
        None => {
            eprintln!("usage: ue_spike discover | ue_spike <gabarit> '<json>'");
            ExitCode::from(2)
        }
    }
}
