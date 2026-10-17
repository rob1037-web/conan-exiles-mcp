//! Serveur MCP (transport stdio) au-dessus du noyau `conan-core`.
//!
//! Chaque outil est une couche fine : toute la logique (sandbox, sauvegarde, diff, validation
//! CSV, listages) vit dans `conan-core`. Configuré uniquement par `MOD_ROOT` (lecture seule)
//! et `CSV_ROOT` (écriture), variables d'environnement.
//!
//! Les erreurs du noyau sont renvoyées comme **erreurs d'outil MCP** (`isError: true`) avec
//! un texte `ERREUR <code>: <message lisible>`, jamais comme panic ni erreur de protocole.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use anyhow::Context;
use conan_core::{Config, CsvStore, ModReader, Newline, StoreError, Table, WriteOptions};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router, ServerHandler,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// Lignes renvoyées par défaut / au maximum par `read_datatable`.
const DEFAULT_ROWS: usize = 500;
const MAX_ROWS: usize = 2000;

#[derive(Clone)]
pub struct ConanServer {
    store: Arc<CsvStore>,
    mod_reader: Arc<ModReader>,
    tool_router: ToolRouter<Self>,
}

/// Où lire : les CSV du serveur (`CSV_ROOT`, défaut) ou le dossier du mod (`MOD_ROOT`, lecture seule).
#[derive(Debug, Default, Clone, Copy, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
pub enum Source {
    #[default]
    Csv,
    Mod,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Source::Csv => "csv",
            Source::Mod => "mod",
        }
    }
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListParams {
    /// "csv" (défaut): fichiers de CSV_ROOT. "mod": dossier du mod, en lecture seule.
    #[serde(default)]
    pub source: Source,
    /// Sous-dossier à lister, relatif à la racine, séparateur "/". Omis: toute la racine.
    pub subfolder: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ReadParams {
    /// Chemin relatif à la racine, séparateur "/", par exemple "datatables/DT_X.csv".
    pub path: String,
    /// "csv" (défaut) ou "mod" (lecture seule).
    #[serde(default)]
    pub source: Source,
    /// Première ligne de données renvoyée (défaut 0).
    pub offset: Option<usize>,
    /// Nombre maximal de lignes renvoyées (défaut 500, maximum 2000).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct WriteParams {
    /// Chemin relatif sous CSV_ROOT, séparateur "/", par exemple "datatables/DT_X.csv".
    pub path: String,
    /// Contenu CSV complet du fichier (la première colonne est la clé de ligne).
    pub content: String,
    /// Défaut TRUE: renvoie seulement l'aperçu (diff), n'écrit rien. Pour écrire pour de
    /// bon, passer explicitement false ET confirm=true.
    pub dry_run: Option<bool>,
    /// Doit être explicitement true, avec dry_run=false, pour écrire réellement. Un fichier
    /// existant est alors sauvegardé dans .backup avant d'être remplacé.
    pub confirm: Option<bool>,
}

fn error_text(e: &StoreError) -> String {
    format!("ERREUR {}: {e}", e.code())
}

/// Exécute un outil en convertissant un éventuel panic en erreur d'outil.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| {
        Err(
            "ERREUR InternalError: erreur interne inattendue; l'état des fichiers n'est pas \
             garanti, vérifier avec read_datatable avant de réessayer"
                .to_owned(),
        )
    })
}

fn newline_name(n: Newline) -> &'static str {
    match n {
        Newline::Lf => "LF",
        Newline::CrLf => "CRLF",
    }
}

impl ConanServer {
    pub fn new(store: CsvStore, mod_reader: ModReader) -> Self {
        ConanServer {
            store: Arc::new(store),
            mod_reader: Arc::new(mod_reader),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl ConanServer {
    #[tool(
        description = "Liste les fichiers de données (récursif, 5000 entrées max). source=csv (défaut): \
                       les CSV gérés par ce serveur (CSV_ROOT). source=mod: le dossier du mod, en lecture \
                       seule. Les sauvegardes (.backup) et fichiers temporaires ne sont jamais listés. \
                       Les chemins renvoyés se réutilisent tels quels dans les autres outils."
    )]
    fn list_mod_files(
        &self,
        Parameters(p): Parameters<ListParams>,
    ) -> Result<CallToolResult, String> {
        guarded(|| {
            let sub = p.subfolder.as_deref();
            let (root, listing) = match p.source {
                Source::Csv => (self.store.csv_root(), self.store.list_files(sub)),
                Source::Mod => (self.mod_reader.root(), self.mod_reader.list_files(sub)),
            };
            let listing = listing.map_err(|e| error_text(&e))?;
            Ok(CallToolResult::structured(json!({
                "source": p.source.name(),
                "root": root.display().to_string(),
                "count": listing.entries.len(),
                "truncated": listing.truncated,
                "files": listing.entries.iter()
                    .map(|f| json!({"path": f.path, "size": f.size}))
                    .collect::<Vec<_>>(),
            })))
        })
    }

    #[tool(
        description = "Lit une DataTable CSV existante et renvoie son en-tête et ses lignes (la première \
                       colonne est la clé de ligne), pour enrichir un fichier ou éviter les doublons. \
                       source=csv (défaut) ou mod (lecture seule). Pagination par offset/limit."
    )]
    fn read_datatable(
        &self,
        Parameters(p): Parameters<ReadParams>,
    ) -> Result<CallToolResult, String> {
        guarded(|| {
            let bytes = match p.source {
                Source::Csv => self.store.read_csv(&p.path),
                Source::Mod => self.mod_reader.read_file(&p.path),
            }
            .map_err(|e| error_text(&e))?;
            let table = Table::parse(&bytes).map_err(|e| {
                format!(
                    "ERREUR InvalidCsv: {:?} n'est pas un CSV lisible: {e}",
                    p.path
                )
            })?;
            let offset = p.offset.unwrap_or(0);
            let limit = p.limit.unwrap_or(DEFAULT_ROWS).min(MAX_ROWS);
            let rows: Vec<Vec<&str>> = table
                .rows
                .iter()
                .skip(offset)
                .take(limit)
                .map(|r| r.cells.iter().map(|c| c.text.as_str()).collect())
                .collect();
            Ok(CallToolResult::structured(json!({
                "path": p.path,
                "source": p.source.name(),
                "columns": table.header.cells.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
                "row_count": table.rows.len(),
                "offset": offset,
                "returned": rows.len(),
                "truncated": offset + rows.len() < table.rows.len(),
                "format": {
                    "bom": table.bom,
                    "newline": newline_name(table.newline),
                    "final_newline": table.final_newline,
                },
                "rows": rows,
            })))
        })
    }

    #[tool(
        description = "Écrit un CSV sous CSV_ROOT (jamais dans le dossier du mod). PAR DÉFAUT dry_run=true: \
                       aperçu (diff: lignes ajoutées/modifiées/supprimées) sans rien écrire. Pour écrire \
                       réellement: dry_run=false ET confirm=true, tous deux explicites. Un fichier existant \
                       est d'abord sauvegardé dans .backup (chemin renvoyé), puis remplacé de façon \
                       atomique. Le contenu doit être un CSV valide."
    )]
    fn write_datatable(
        &self,
        Parameters(p): Parameters<WriteParams>,
    ) -> Result<CallToolResult, String> {
        guarded(|| {
            let dry_run = p.dry_run.unwrap_or(true);
            let confirm = p.confirm.unwrap_or(false);
            if !dry_run && !confirm {
                return Err("ERREUR ConfirmRequired: écriture réelle refusée. Il faut dry_run=false ET \
                            confirm=true, explicitement. Rien n'a été écrit; relancer d'abord en dry_run \
                            (défaut) pour voir le diff."
                    .to_owned());
            }
            let outcome = self
                .store
                .write_csv(
                    &p.path,
                    p.content.as_bytes(),
                    WriteOptions { dry_run, confirm },
                )
                .map_err(|e| error_text(&e))?;

            let backup_rel = outcome.backup.as_ref().map(|b| {
                b.strip_prefix(self.store.csv_root())
                    .unwrap_or(b)
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            });
            let status = if outcome.unchanged {
                "AUCUN CHANGEMENT (contenu identique, rien n'a été écrit)"
            } else if outcome.applied {
                "ÉCRIT"
            } else {
                "DRY-RUN (aperçu seulement, rien n'a été écrit)"
            };
            let mut report = format!(
                "{status}\nFichier: {} ({})\nChemin réel: {}\n{}",
                p.path,
                if outcome.existed {
                    "existant"
                } else {
                    "nouveau"
                },
                outcome.path.display(),
                outcome.diff.render(),
            );
            if let (Some(b), Some(rel)) = (&outcome.backup, &backup_rel) {
                report.push_str(&format!(
                    "Sauvegarde créée: {rel}\nChemin réel de la sauvegarde: {}\n",
                    b.display()
                ));
            }
            if !outcome.applied && !outcome.unchanged {
                report.push_str(
                    "Pour écrire réellement: rappeler avec dry_run=false et confirm=true.\n",
                );
            }
            let mut result = CallToolResult::success(vec![ContentBlock::text(report)]);
            result.structured_content = Some(structured_write(&p.path, &outcome, backup_rel));
            Ok(result)
        })
    }
}

fn structured_write(
    path: &str,
    outcome: &conan_core::WriteOutcome,
    backup_rel: Option<String>,
) -> Value {
    json!({
        "path": path,
        "real_path": outcome.path.display().to_string(),
        "existed": outcome.existed,
        "applied": outcome.applied,
        "unchanged": outcome.unchanged,
        "dry_run": !outcome.applied && !outcome.unchanged,
        "backup": backup_rel,
        "backup_real_path": outcome.backup.as_ref().map(|b| b.display().to_string()),
        "diff": {
            "added": outcome.diff.added.len(),
            "modified": outcome.diff.modified.len(),
            "removed": outcome.diff.removed.len(),
            "header_changed": outcome.diff.header_changed,
            "format_changed": outcome.diff.format_changed,
            "order_changed": outcome.diff.order_changed,
            "summary": outcome.diff.summary(),
        },
    })
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ConanServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "conan-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Données pour le Devkit Conan Exiles. Les CSV s'écrivent uniquement sous CSV_ROOT; \
                 le dossier du mod (MOD_ROOT) est en lecture seule. Toute écriture commence par un \
                 aperçu (dry_run, défaut): présentez le diff avant de demander dry_run=false + \
                 confirm=true. L'import CSV -> DataTable reste manuel dans le Devkit.",
            )
    }
}

/// Construit le serveur depuis une configuration. Échoue avec un message clair si
/// `MOD_ROOT` est inutilisable ou si `CSV_ROOT` est situé sous lui. Crée `CSV_ROOT` au besoin.
pub fn build_server(config: &Config) -> anyhow::Result<ConanServer> {
    let context = || {
        format!(
            "configuration invalide (MOD_ROOT = {:?}, CSV_ROOT = {:?})",
            config.mod_root, config.csv_root
        )
    };
    let store = CsvStore::open(config).with_context(context)?;
    let mod_reader = ModReader::open(&config.mod_root).with_context(context)?;
    Ok(ConanServer::new(store, mod_reader))
}

/// Lit `MOD_ROOT` et `CSV_ROOT` dans l'environnement (ou dans `get`) puis construit le serveur.
pub fn build_server_from_vars(
    get: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> anyhow::Result<ConanServer> {
    let config = Config::from_vars(get).context("configuration invalide")?;
    build_server(&config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conan_core::SandboxError;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// Chaque erreur du noyau a un code stable et un message lisible, sans panic.
    #[test]
    fn every_core_error_maps_to_a_coded_readable_text() {
        let parse_err = conan_core::Table::parse(b"\"x").unwrap_err();
        let cases: Vec<(StoreError, &str)> = vec![
            (
                StoreError::ModReadOnly {
                    path: p("m/x"),
                    mod_root: p("m"),
                },
                "ModReadOnly",
            ),
            (
                StoreError::AlreadyExists { path: p("a.csv") },
                "AlreadyExists",
            ),
            (
                StoreError::BackupFailed {
                    path: p("a.csv"),
                    reason: "r".into(),
                },
                "BackupFailed",
            ),
            (
                StoreError::InvalidCsv {
                    source: parse_err.clone(),
                },
                "InvalidCsv",
            ),
            (
                StoreError::ExistingUnreadable {
                    path: p("a.csv"),
                    source: parse_err,
                },
                "ExistingUnreadable",
            ),
            (
                StoreError::ReservedPath {
                    path: ".backup/x".into(),
                },
                "ReservedPath",
            ),
            (
                StoreError::TooLarge {
                    path: p("big"),
                    size: 2,
                    max: 1,
                },
                "TooLarge",
            ),
            (
                StoreError::CsvRootInsideMod {
                    csv_root: p("c"),
                    mod_root: p("m"),
                },
                "CsvRootInsideMod",
            ),
            (
                StoreError::Sandbox(SandboxError::Escapes {
                    path: "l/x".into(),
                    resolved: p("/o/x"),
                    root: p("/r"),
                }),
                "PathEscapes",
            ),
            (
                StoreError::Sandbox(SandboxError::NotFound {
                    path: "x".into(),
                    root: p("/r"),
                }),
                "NotFound",
            ),
        ];
        for (err, code) in cases {
            let text = error_text(&err);
            assert!(text.starts_with(&format!("ERREUR {code}: ")), "{text}");
            assert!(text.len() > code.len() + 20, "message trop pauvre: {text}");
        }
    }

    #[test]
    fn a_panic_inside_a_tool_becomes_a_tool_error() {
        let r: Result<(), String> = guarded(|| panic!("boom"));
        assert!(r.unwrap_err().starts_with("ERREUR InternalError:"));
    }
}
