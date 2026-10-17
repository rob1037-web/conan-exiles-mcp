//! Point d'entrée : serveur MCP en stdio. Rien d'autre que le protocole n'est écrit sur
//! stdout ; les messages de démarrage et d'erreur vont sur stderr.

use std::process::ExitCode;

use mcp_server::build_server_from_vars;
use rmcp::{transport::stdio, ServiceExt};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mcp-server: démarrage refusé: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let server = build_server_from_vars(|name| std::env::var_os(name))?;
    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}
