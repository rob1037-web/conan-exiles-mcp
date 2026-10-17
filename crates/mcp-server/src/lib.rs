//! Serveur MCP (transport stdio) au-dessus du noyau `conan-core`.
//!
//! Chaque outil est une couche fine : toute la logique (sandbox, sauvegarde, diff, validation
//! CSV, listages) vit dans `conan-core`. Configuré uniquement par `MOD_ROOT` (lecture seule)
//! et `CSV_ROOT` (écriture), plus `SCHEMA_ROOT` (schémas, lecture seule, optionnel),
//! variables d'environnement. Sans schéma valide, aucune écriture.
//!
//! Les erreurs du noyau sont renvoyées comme **erreurs d'outil MCP** (`isError: true`) avec
//! un texte `ERREUR <code>: <message lisible>`, jamais comme panic ni erreur de protocole.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use anyhow::Context;
use conan_core::{
    render_report, validate_table, Config, CsvStore, FieldType, ModReader, Newline, Schema,
    SchemaStore, StoreError, Table, ValidationError, WriteOptions,
};
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
    /// Les schémas, ou la raison pour laquelle ils sont indisponibles (écritures refusées).
    schemas: Arc<Result<SchemaStore, String>>,
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
    /// Nom de la table, qui désigne son schéma dans SCHEMA_ROOT (ex. "DT_Purge_Nordheimer").
    /// Sans schéma valide, aucune écriture.
    pub table: String,
    /// Contenu CSV complet du fichier, exactement au format de l'export du Devkit
    /// (en-tête identique au schéma, première colonne = clé de ligne).
    pub content: String,
    /// Chemin relatif sous CSV_ROOT, séparateur "/". Défaut: "<table>.csv".
    pub path: Option<String>,
    /// Défaut TRUE: renvoie seulement l'aperçu (diff + rapport de validation), n'écrit rien.
    /// Pour écrire pour de bon, passer explicitement false ET confirm=true.
    pub dry_run: Option<bool>,
    /// Doit être explicitement true, avec dry_run=false, pour écrire réellement. Un fichier
    /// existant est alors sauvegardé dans .backup avant d'être remplacé.
    pub confirm: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DescribeParams {
    /// Nom de la table (ex. "DT_Purge_Nordheimer").
    pub table: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ValidateParams {
    /// Nom de la table (ex. "DT_Purge_Nordheimer").
    pub table: String,
    /// Lignes à valider: chacune est la liste de textes [clé, valeur de chaque colonne du
    /// schéma, dans l'ordre]. Omis: valide le CSV existant dans CSV_ROOT (diagnostic de
    /// corruption, sans réparation).
    pub rows: Option<Vec<Vec<String>>>,
    /// Chemin du CSV existant sous CSV_ROOT, si `rows` est omis. Défaut: "<table>.csv".
    pub path: Option<String>,
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

/// Chemin affichable: sans le préfixe verbatim `\\?\` que Windows ajoute aux chemins canoniques.
fn pretty(path: &std::path::Path) -> String {
    let s = path.display().to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        s
    }
}

fn newline_name(n: Newline) -> &'static str {
    match n {
        Newline::Lf => "LF",
        Newline::CrLf => "CRLF",
    }
}

impl ConanServer {
    pub fn new(
        store: CsvStore,
        mod_reader: ModReader,
        schemas: Result<SchemaStore, String>,
    ) -> Self {
        ConanServer {
            store: Arc::new(store),
            mod_reader: Arc::new(mod_reader),
            schemas: Arc::new(schemas),
            tool_router: Self::tool_router(),
        }
    }

    /// Le magasin de schémas, ou une erreur d'outil explicite s'il est indisponible.
    fn schema_store(&self) -> Result<&SchemaStore, String> {
        self.schemas.as_ref().as_ref().map_err(|reason| {
            format!(
                "ERREUR SchemaRootUnavailable: {reason}. Sans SCHEMA_ROOT valide, toute écriture est \
                 refusée (la lecture reste possible)."
            )
        })
    }

    /// Charge le schéma d'une table (erreur bloquante s'il est absent ou malformé).
    fn schema_for(&self, table: &str) -> Result<Schema, String> {
        self.schema_store()?
            .load(table)
            .map_err(|e| format!("ERREUR {}: {e}", e.code()))
    }
}

fn error_json(e: &ValidationError) -> Value {
    json!({
        "line": e.line,
        "key": e.key,
        "column": e.column,
        "value": e.received,
        "rule": e.rule.code(),
        "expected": e.expected,
        "schema_drift": e.rule.is_schema_drift(),
    })
}

/// Nombre d'erreurs détaillées dans le texte (le contenu structuré les contient toutes).
const TEXT_ERRORS: usize = 200;

fn report_text(errors: &[ValidationError]) -> String {
    let mut out = render_report(errors, TEXT_ERRORS);
    if errors.len() > TEXT_ERRORS {
        out.push_str("  (liste complète dans le contenu structuré)\n");
    }
    out
}

fn describe_field(f: &conan_core::Field) -> Value {
    let mut v = json!({"name": f.name, "required": f.required});
    let (kind, extra) = match &f.ty {
        FieldType::String { min_len, max_len } => {
            ("string", json!({"min_len": min_len, "max_len": max_len}))
        }
        FieldType::Int { min, max } => ("int", json!({"min": min, "max": max})),
        FieldType::Float { min, max } => ("float", json!({"min": min, "max": max})),
        FieldType::Bool => ("bool", json!({})),
        FieldType::Enum { values } => ("enum", json!({"values": values})),
        FieldType::AssetRef => ("asset_ref", json!({})),
        FieldType::Raw => ("raw", json!({})),
        _ => ("unknown", json!({})),
    };
    v["type"] = json!(kind);
    if let (Some(obj), Some(extra)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in extra {
            if !val.is_null() {
                obj.insert(k.clone(), val.clone());
            }
        }
    }
    v
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
                "root": pretty(root),
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
        description = "Liste les schémas de tables disponibles (SCHEMA_ROOT, lecture seule), avec leur \
                       validité: un schéma malformé est signalé avec le fichier et le champ fautif. \
                       Pas de schéma valide = pas d'écriture pour cette table."
    )]
    fn list_schemas(&self) -> Result<CallToolResult, String> {
        guarded(|| {
            let store = self.schema_store()?;
            let entries = store
                .list()
                .map_err(|e| format!("ERREUR {}: {e}", e.code()))?;
            let schemas: Vec<Value> = entries
                .iter()
                .map(|e| match &e.status {
                    Ok(s) => json!({
                        "table": e.table, "file": e.file, "valid": true,
                        "columns": s.fields.len(),
                        "description": s.description,
                    }),
                    Err(err) => json!({
                        "table": e.table, "file": e.file, "valid": false,
                        "error_code": err.code(), "error": err.to_string(),
                    }),
                })
                .collect();
            Ok(CallToolResult::structured(json!({
                "schema_root": pretty(store.root()),
                "count": schemas.len(),
                "schemas": schemas,
            })))
        })
    }

    #[tool(
        description = "Décrit le schéma d'une table: en-tête CSV attendu (colonne clé puis colonnes dans \
                       l'ordre), type, caractère obligatoire, bornes et valeurs d'enum de chaque colonne."
    )]
    fn describe_schema(
        &self,
        Parameters(p): Parameters<DescribeParams>,
    ) -> Result<CallToolResult, String> {
        guarded(|| {
            let schema = self.schema_for(&p.table)?;
            let key = schema.key_column.clone();
            let header: Vec<String> = std::iter::once(key.clone())
                .chain(schema.fields.iter().map(|f| f.name.clone()))
                .collect();
            Ok(CallToolResult::structured(json!({
                "table": schema.name,
                "description": schema.description,
                "key_column": key,
                "csv_header": header.join(","),
                "default_path": format!("{}.csv", schema.name),
                "columns": schema.fields.iter().map(describe_field).collect::<Vec<_>>(),
            })))
        })
    }

    #[tool(
        description = "Valide des lignes contre le schéma d'une table, sans rien écrire. `rows`: liste de \
                       lignes [clé, valeur de chaque colonne dans l'ordre du schéma]. Sans `rows`, valide le \
                       CSV existant dans CSV_ROOT (diagnostic de corruption, sans réparation). Toutes les \
                       erreurs sont listées: ligne, clé, colonne, valeur, règle violée. Une différence \
                       d'en-tête avec le schéma est une dérive de schéma."
    )]
    fn validate_rows(
        &self,
        Parameters(p): Parameters<ValidateParams>,
    ) -> Result<CallToolResult, String> {
        guarded(|| {
            let schema = self.schema_for(&p.table)?;
            let (table, source, path) = match &p.rows {
                Some(rows) => (Table::for_schema(&schema, rows), "rows", None),
                None => {
                    let path = p
                        .path
                        .clone()
                        .unwrap_or_else(|| format!("{}.csv", schema.name));
                    let bytes = self.store.read_csv(&path).map_err(|e| error_text(&e))?;
                    let table = Table::parse(&bytes).map_err(|e| {
                        format!(
                            "ERREUR InvalidCsv: {path:?} n'est pas un CSV lisible (corruption du fichier): {e}"
                        )
                    })?;
                    (Ok(table), "csv", Some(path))
                }
            };
            let (errors, row_count) = match table {
                Ok(t) => (
                    validate_table(&schema, &t).err().unwrap_or_default(),
                    t.rows.len(),
                ),
                Err(errors) => (errors, p.rows.as_ref().map_or(0, Vec::len)),
            };
            let drift = errors.iter().any(|e| e.rule.is_schema_drift());
            let head = if errors.is_empty() {
                format!(
                    "VALIDATION OK: table {} ({row_count} ligne(s))\n",
                    schema.name
                )
            } else {
                format!(
                    "VALIDATION ÉCHOUÉE: table {}, {} erreur(s){}\n{}",
                    schema.name,
                    errors.len(),
                    if drift {
                        " — DÉRIVE DE SCHÉMA (l'en-tête ne correspond pas au schéma)"
                    } else {
                        ""
                    },
                    report_text(&errors),
                )
            };
            let mut result = CallToolResult::success(vec![ContentBlock::text(head)]);
            result.structured_content = Some(json!({
                "table": schema.name,
                "source": source,
                "path": path,
                "valid": errors.is_empty(),
                "schema_drift": drift,
                "row_count": row_count,
                "error_count": errors.len(),
                "errors": errors.iter().map(error_json).collect::<Vec<_>>(),
            }));
            Ok(result)
        })
    }

    #[tool(
        description = "Écrit un CSV sous CSV_ROOT (jamais dans le dossier du mod), VALIDÉ contre le schéma de \
                       la table (SCHEMA_ROOT): sans schéma valide, aucune écriture, et aucun mode forcer. \
                       PAR DÉFAUT dry_run=true: aperçu (diff par clé + rapport de validation) sans rien \
                       écrire. Pour écrire réellement: dry_run=false ET confirm=true, tous deux explicites, \
                       et le contenu doit être valide. Un fichier existant est d'abord sauvegardé dans \
                       .backup (chemin renvoyé), puis remplacé de façon atomique."
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
            // Pas de schéma valide, pas d'écriture: même en dry-run.
            let schema = self.schema_for(&p.table)?;
            let path = p
                .path
                .clone()
                .unwrap_or_else(|| format!("{}.csv", schema.name));
            let written = match self.store.write_with_schema(
                &schema,
                &path,
                p.content.as_bytes(),
                WriteOptions { dry_run, confirm },
            ) {
                Ok(w) => w,
                Err(StoreError::ValidationFailed { table, errors }) => {
                    let drift = errors.iter().any(|e| e.rule.is_schema_drift());
                    let code = if drift {
                        "SchemaDrift"
                    } else {
                        "ValidationFailed"
                    };
                    let text = format!(
                        "ERREUR {code}: écriture refusée pour la table {table}, {} erreur(s) de validation \
                         ({}). Rien n'a été écrit. Il n'existe aucun mode pour forcer.\n{}",
                        errors.len(),
                        if drift { "dérive de schéma: l'en-tête ne correspond pas au schéma" } else { "valeurs invalides" },
                        report_text(&errors),
                    );
                    let mut result = CallToolResult::error(vec![ContentBlock::text(text)]);
                    result.structured_content = Some(json!({
                        "table": table, "valid": false, "schema_drift": drift,
                        "error_count": errors.len(),
                        "errors": errors.iter().map(error_json).collect::<Vec<_>>(),
                    }));
                    return Ok(result);
                }
                Err(e) => return Err(error_text(&e)),
            };
            let (outcome, validation) = (written.outcome, written.report);

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
            } else if validation.is_empty() {
                "DRY-RUN (aperçu seulement, rien n'a été écrit)"
            } else {
                "DRY-RUN — VALIDATION ÉCHOUÉE (aperçu seulement, rien n'a été écrit; l'écriture réelle serait refusée)"
            };
            let mut report = format!(
                "{status}\nTable: {} — fichier: {} ({})\nChemin réel: {}\n{}",
                schema.name,
                path,
                if outcome.existed {
                    "existant"
                } else {
                    "nouveau"
                },
                pretty(&outcome.path),
                outcome.diff.render(),
            );
            if validation.is_empty() {
                report.push_str("Validation: OK\n");
            } else {
                report.push_str(&format!(
                    "Validation: {} erreur(s){}\n{}",
                    validation.len(),
                    if validation.iter().any(|e| e.rule.is_schema_drift()) {
                        " — DÉRIVE DE SCHÉMA"
                    } else {
                        ""
                    },
                    report_text(&validation),
                ));
            }
            if let (Some(b), Some(rel)) = (&outcome.backup, &backup_rel) {
                report.push_str(&format!(
                    "Sauvegarde créée: {rel}\nChemin réel de la sauvegarde: {}\n",
                    pretty(b)
                ));
            }
            if !outcome.applied && !outcome.unchanged && validation.is_empty() {
                report.push_str(
                    "Pour écrire réellement: rappeler avec dry_run=false et confirm=true.\n",
                );
            }
            let mut result = CallToolResult::success(vec![ContentBlock::text(report)]);
            let mut structured = structured_write(&path, &outcome, backup_rel);
            structured["table"] = json!(schema.name);
            structured["valid"] = json!(validation.is_empty());
            structured["schema_drift"] = json!(validation.iter().any(|e| e.rule.is_schema_drift()));
            structured["validation_errors"] =
                json!(validation.iter().map(error_json).collect::<Vec<_>>());
            result.structured_content = Some(structured);
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
        "real_path": pretty(&outcome.path),
        "existed": outcome.existed,
        "applied": outcome.applied,
        "unchanged": outcome.unchanged,
        "dry_run": !outcome.applied && !outcome.unchanged,
        "backup": backup_rel,
        "backup_real_path": outcome.backup.as_ref().map(|b| pretty(b)),
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
                 le dossier du mod (MOD_ROOT) et les schémas (SCHEMA_ROOT) sont en lecture seule. \
                 Toute écriture est validée contre le schéma de la table (list_schemas, \
                 describe_schema, validate_rows) et commence par un aperçu (dry_run, défaut): \
                 présentez le diff et le rapport de validation avant de demander dry_run=false + \
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
    // SCHEMA_ROOT absent ou invalide n'empêche pas de démarrer: la lecture fonctionne, toute
    // écriture est refusée avec la raison.
    let schemas = match &config.schema_root {
        None => Err("SCHEMA_ROOT n'est pas configuré".to_owned()),
        Some(root) => SchemaStore::open(root).map_err(|e| e.to_string()),
    };
    if let Err(reason) = &schemas {
        eprintln!("mcp-server: avertissement: {reason}; les écritures seront refusées.");
    }
    Ok(ConanServer::new(store, mod_reader, schemas))
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
                StoreError::SchemaReadOnly {
                    path: p("s/x"),
                    schema_root: p("s"),
                },
                "SchemaReadOnly",
            ),
            (
                StoreError::ValidationFailed {
                    table: "DT_X".into(),
                    errors: vec![],
                },
                "ValidationFailed",
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
    fn verbatim_prefixes_are_hidden_from_displayed_paths() {
        assert_eq!(pretty(&p(r"\\?\D:\ConanMod\csv")), r"D:\ConanMod\csv");
        assert_eq!(pretty(&p(r"\\?\UNC\srv\share\x")), r"\\srv\share\x");
        assert_eq!(pretty(&p("/home/x")), "/home/x");
    }

    #[test]
    fn a_panic_inside_a_tool_becomes_a_tool_error() {
        let r: Result<(), String> = guarded(|| panic!("boom"));
        assert!(r.unwrap_err().starts_with("ERREUR InternalError:"));
    }
}
