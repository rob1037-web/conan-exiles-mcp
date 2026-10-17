//! Serveur MCP (transport stdio) au-dessus du noyau `conan-core`.
//!
//! Chaque outil est une couche fine : toute la logique (sandbox, sauvegarde, diff, validation
//! CSV) vit dans `conan-core`. Configuré uniquement par `MOD_ROOT` (lecture seule) et
//! `CSV_ROOT` (écriture), variables d'environnement.

use std::sync::Arc;

use anyhow::Context;
use conan_core::{Config, CsvStore};
use rmcp::{
    handler::server::router::tool::ToolRouter,
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool_handler, tool_router, ServerHandler,
};

#[derive(Clone)]
pub struct ConanServer {
    #[allow(dead_code)] // utilisé par les outils
    store: Arc<CsvStore>,
    tool_router: ToolRouter<Self>,
}

impl ConanServer {
    pub fn new(store: CsvStore) -> Self {
        ConanServer {
            store: Arc::new(store),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router(allow_empty)]
impl ConanServer {}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ConanServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "conan-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
            "Serveur de données pour le Devkit Conan Exiles. Les CSV s'écrivent uniquement sous \
             CSV_ROOT; le dossier du mod (MOD_ROOT) est en lecture seule.",
        )
    }
}

/// Construit le serveur depuis une configuration. Échoue avec un message clair si
/// `MOD_ROOT` est inutilisable ou si `CSV_ROOT` est situé sous lui. Crée `CSV_ROOT` au besoin.
pub fn build_server(config: &Config) -> anyhow::Result<ConanServer> {
    let store = CsvStore::open(config).with_context(|| {
        format!(
            "configuration invalide (MOD_ROOT = {:?}, CSV_ROOT = {:?})",
            config.mod_root, config.csv_root
        )
    })?;
    Ok(ConanServer::new(store))
}

/// Lit `MOD_ROOT` et `CSV_ROOT` dans l'environnement (ou dans `get`) puis construit le serveur.
pub fn build_server_from_vars(
    get: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> anyhow::Result<ConanServer> {
    let config = Config::from_vars(get).context("configuration invalide")?;
    build_server(&config)
}
