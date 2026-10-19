//! Serveur MCP (transport stdio) au-dessus du noyau `conan-core`.
//!
//! Chaque outil est une couche fine : toute la logique (sandbox, sauvegarde, diff, validation
//! CSV, listages) vit dans `conan-core`. Configuré uniquement par `MOD_ROOT` (lecture seule)
//! et `CSV_ROOT` (écriture), plus `SCHEMA_ROOT` (schémas, lecture seule, optionnel),
//! variables d'environnement. Sans schéma valide, aucune écriture.
//!
//! Les erreurs du noyau sont renvoyées comme **erreurs d'outil MCP** (`isError: true`) avec
//! un texte `ERREUR <code>: <message lisible>`, jamais comme panic ni erreur de protocole.

pub mod ue_bridge;
pub mod ue_defaults;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use anyhow::Context;
use conan_core::{
    render_report, validate_table, Config, CsvStore, FieldType, ModReader, Newline, RowOp, Schema,
    SchemaStore, SchemaWrite, StoreError, Table, ValidationError, WriteOptions,
};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router, ServerHandler,
};
use serde::Deserialize;
use serde_json::{json, Value};
use ue_bridge::{templates, BridgeConfig, TemplateOutput, UeError};

/// Lignes renvoyées par défaut / au maximum par `read_datatable`.
const DEFAULT_ROWS: usize = 500;
const MAX_ROWS: usize = 2000;

#[derive(Clone)]
pub struct ConanServer {
    store: Arc<CsvStore>,
    mod_reader: Arc<ModReader>,
    /// Les schémas, ou la raison pour laquelle ils sont indisponibles (écritures refusées).
    schemas: Arc<Result<SchemaStore, String>>,
    /// Pont vers le Devkit (Phase 4a, lecture seule).
    ue: Arc<BridgeConfig>,
    /// Une seule session Python Remote Execution à la fois (une session par appel d'outil).
    ue_lock: Arc<tokio::sync::Mutex<()>>,
    /// Garde-fou EditorBusy: la raison de la dernière commande restée en doute (délai dépassé ou
    /// rupture de protocole après l'envoi). Tant qu'elle est posée, seuls `ue_status` et les outils
    /// non-ue passent; seul un `ue_status` réussi la lève.
    ue_suspect: Arc<std::sync::Mutex<Option<String>>>,
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

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct AddRowParams {
    /// Nom de la table (ex. "DT_Purge_Nordheimer"), qui désigne son schéma.
    pub table: String,
    /// Clé de la nouvelle ligne (Row Name). Ne doit pas exister déjà (casse ignorée). Sa casse
    /// est conservée telle que fournie.
    pub key: String,
    /// TOUTES les colonnes du schéma (hors colonne clé), nom de colonne -> valeur (texte, nombre
    /// ou booléen). Une colonne inconnue ou manquante est refusée.
    pub values: std::collections::BTreeMap<String, Value>,
    /// Chemin du CSV sous CSV_ROOT. Défaut: "<table>.csv". Le fichier doit déjà exister.
    pub path: Option<String>,
    /// Défaut TRUE: aperçu seulement (diff + validation). Écrire pour de bon: false ET confirm=true.
    pub dry_run: Option<bool>,
    /// Doit être explicitement true, avec dry_run=false, pour écrire réellement.
    pub confirm: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UpdateRowParams {
    /// Nom de la table (ex. "DT_Purge_Nordheimer"), qui désigne son schéma.
    pub table: String,
    /// Clé de la ligne à modifier. Doit exister (casse ignorée). La clé n'est pas modifiable.
    pub key: String,
    /// Seulement les colonnes à changer, nom de colonne -> nouvelle valeur (texte, nombre ou
    /// booléen). Les autres colonnes restent inchangées. La colonne clé est refusée.
    pub values: std::collections::BTreeMap<String, Value>,
    /// Chemin du CSV sous CSV_ROOT. Défaut: "<table>.csv".
    pub path: Option<String>,
    /// Défaut TRUE: aperçu seulement (diff + validation). Écrire pour de bon: false ET confirm=true.
    pub dry_run: Option<bool>,
    /// Doit être explicitement true, avec dry_run=false, pour écrire réellement.
    pub confirm: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DeleteRowParams {
    /// Nom de la table (ex. "DT_Purge_Nordheimer"), qui désigne son schéma.
    pub table: String,
    /// Clé de la ligne à supprimer. Doit exister (casse ignorée).
    pub key: String,
    /// Chemin du CSV sous CSV_ROOT. Défaut: "<table>.csv".
    pub path: Option<String>,
    /// Défaut TRUE: montre la ligne qui serait supprimée, sans rien écrire. Supprimer pour de
    /// bon: false ET confirm=true.
    pub dry_run: Option<bool>,
    /// Doit être explicitement true, avec dry_run=false, pour supprimer réellement.
    pub confirm: Option<bool>,
}

/// Convertit les valeurs JSON d'un outil par ligne en texte (texte, nombre ou booléen seulement).
fn values_to_text(
    values: &std::collections::BTreeMap<String, Value>,
) -> Result<Vec<(String, String)>, String> {
    values
        .iter()
        .map(|(col, v)| match v {
            Value::String(s) => Ok((col.clone(), s.clone())),
            Value::Number(n) => Ok((col.clone(), n.to_string())),
            Value::Bool(b) => Ok((col.clone(), b.to_string())),
            other => Err(format!(
                "ERREUR InvalidParameter: la valeur de la colonne {col:?} doit être un texte, un nombre \
                 ou un booléen (reçu {other}). Rien n'a été écrit."
            )),
        })
        .collect()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UeSearchParams {
    /// Préfixe du dossier de l'asset, avec ou sans "/Game" (ex. "Mods/test", "/Game/Items").
    pub path_prefix: Option<String>,
    /// Nom exact de la classe, casse ignorée (ex. "DataTable", "Blueprint", "StaticMesh").
    pub class_name: Option<String>,
    /// Sous-chaîne du nom de l'asset, casse ignorée.
    pub name_contains: Option<String>,
    /// Nombre maximal de résultats renvoyés (défaut 50, maximum 500).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UeAssetParams {
    /// Chemin d'objet de l'asset, tel que renvoyé par ue_search_assets
    /// (ex. "/Game/Mods/test/test/Mailbox/BP_PL_Mailbox.BP_PL_Mailbox").
    pub object_path: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UeBlueprintParams {
    /// Chemin d'objet du Blueprint (ex. "/Game/Mods/test/test/Mailbox/BP_PL_Mailbox.BP_PL_Mailbox").
    pub object_path: String,
    /// Lister aussi les variables héritées des classes parentes (défaut false: seulement celles
    /// déclarées dans ce Blueprint).
    pub include_inherited: Option<bool>,
    /// Reprise: indice de départ dans la liste des variables (le champ `next_index` de la réponse
    /// précédente). Une commande lit au plus 60 variables et s'arrête après 2 s.
    pub start: Option<usize>,
    /// Noms exacts des variables dont le conteneur/struct doit être déplié (20 éléments au plus).
    pub expand: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UeDefaultsParams {
    /// Chemin d'objet du Blueprint (ex. "/Game/Mods/test/test/Mailbox/BP_PL_Mailbox.BP_PL_Mailbox").
    pub object_path: String,
    /// "members" (défaut: variables et propriétés héritées, valeurs lues sur le CDO), "components"
    /// (liste des composants, hérités compris) ou "component_values" (propriétés d'UN composant,
    /// champ `component` obligatoire).
    pub section: Option<String>,
    /// Nom de variable du composant pour section="component_values" (voir section="components").
    pub component: Option<String>,
    /// Noms EXACTS des propriétés à lire (200 au plus). Sans ce champ: tous les membres.
    pub names: Option<Vec<String>>,
    /// Noms exacts des propriétés dont le tableau/set/map/struct doit être déplié (20 au plus,
    /// 20 éléments par conteneur). Par défaut les conteneurs et structs ne sont PAS dépliés.
    pub expand: Option<Vec<String>>,
    /// Reprise: `next_index` d'une réponse précédente incomplète.
    pub start: Option<usize>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UeDiffParams {
    /// Blueprint à lire (chemin d'objet). Dans le mode « valeurs proposées », c'est le Blueprint dont on
    /// compare les valeurs actuelles aux valeurs de `proposed` (alias accepté: `asset_path`).
    #[serde(alias = "asset_path")]
    pub object_path_a: String,
    /// Mode « deux Blueprints »: second Blueprint (chemin d'objet). Exclusif avec `proposed`.
    pub object_path_b: Option<String>,
    /// Mode « valeurs proposées » (dry-run, rien n'est écrit): `{nom exact de variable: valeur}`, 200 noms
    /// au plus. Exclusif avec `object_path_b`; seule la section "members" est comparée.
    pub proposed: Option<serde_json::Map<String, Value>>,
    /// "members" (défaut), "components" ou "component_values" (voir ue_get_defaults). Mode deux Blueprints.
    pub section: Option<String>,
    /// Composant à comparer pour section="component_values". Mode deux Blueprints.
    pub component: Option<String>,
    /// Noms exacts des propriétés à comparer (200 au plus). Sans ce champ: tous les membres. Mode deux Blueprints.
    pub names: Option<Vec<String>>,
    /// Noms exacts des propriétés à déplier des deux côtés (20 au plus). Mode deux Blueprints.
    pub expand: Option<Vec<String>>,
}

/// Commandes envoyées à l'éditeur au plus par appel d'outil (chacune: un chargement, 60 éléments, 2 s).
const UE_MAX_COMMANDS: usize = 12;
const UE_MAX_NAMES: usize = 200;
const UE_MAX_EXPAND: usize = 20;
const UE_SECTIONS: [&str; 3] = ["members", "components", "component_values"];

const UE_DEFAULT_LIMIT: usize = 50;
const UE_MAX_LIMIT: usize = 500;
const UE_MAX_TEXT: usize = 512;
const UE_MAX_TRACEBACK: usize = 4000;

fn ue_error_text(e: &UeError) -> String {
    match e {
        UeError::PythonError(trace) => {
            let shown: String = trace.chars().take(UE_MAX_TRACEBACK).collect();
            format!("ERREUR {}: erreur Python dans l'éditeur (lecture seule, rien n'a été modifié):\n{shown}", e.code())
        }
        other => format!("ERREUR {}: {other}", other.code()),
    }
}

/// Retire les clés racine à `null` (diagnostics absents) pour garder le JSON compact.
fn without_nulls(mut value: Value) -> Value {
    if let Some(obj) = value.as_object_mut() {
        obj.retain(|_, v| !v.is_null());
    }
    value
}

fn ue_text_param(name: &str, value: &Option<String>) -> Result<Option<String>, String> {
    match value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) if v.chars().count() > UE_MAX_TEXT => Err(format!(
            "ERREUR InvalidParameter: {name} dépasse {UE_MAX_TEXT} caractères."
        )),
        Some(v) => Ok(Some(v.to_owned())),
    }
}

/// Liste de noms de propriétés: bornée, sans vide ni doublon silencieux.
fn ue_names_param(
    name: &str,
    value: &Option<Vec<String>>,
    max: usize,
) -> Result<Option<Vec<String>>, String> {
    let Some(list) = value else { return Ok(None) };
    if list.len() > max {
        return Err(format!(
            "ERREUR InvalidParameter: {name} contient {} noms, maximum {max}.",
            list.len()
        ));
    }
    let mut out: Vec<String> = Vec::new();
    for raw in list {
        let n = raw.trim();
        if n.is_empty() || n.chars().count() > 128 || n.chars().any(char::is_control) {
            return Err(format!(
                "ERREUR InvalidParameter: {name} contient un nom invalide ({raw:?}); \
                 attendu: un nom exact de propriété (non vide, 128 caractères au plus)."
            ));
        }
        if !out.iter().any(|o| o == n) {
            out.push(n.to_owned());
        }
    }
    Ok(Some(out))
}

/// Paramètres communs de lecture des défauts, validés, sous la forme attendue par le gabarit.
fn ue_defaults_base(
    object_path: &str,
    section: &Option<String>,
    component: &Option<String>,
    names: &Option<Vec<String>>,
    expand: &Option<Vec<String>>,
) -> Result<Value, String> {
    let object_path = normalize_object_path(object_path)?;
    let section = section.as_deref().map(str::trim).unwrap_or("members");
    if !UE_SECTIONS.contains(&section) {
        return Err(format!(
            "ERREUR InvalidParameter: section {section:?} inconnue (attendu: {}).",
            UE_SECTIONS.join(", ")
        ));
    }
    let component = ue_text_param("component", component)?;
    if section == "component_values" && component.is_none() {
        return Err(
            "ERREUR InvalidParameter: section=\"component_values\" exige `component` (nom de variable \
             du composant, voir section=\"components\")."
                .into(),
        );
    }
    let mut base = json!({"object_path": object_path, "section": section});
    if let Some(c) = component {
        base["component"] = json!(c);
    }
    if let Some(n) = ue_names_param("names", names, UE_MAX_NAMES)? {
        base["names"] = json!(n);
    }
    if let Some(e) = ue_names_param("expand", expand, UE_MAX_EXPAND)? {
        base["expand"] = json!(e);
    }
    Ok(base)
}

/// Résultat de la lecture paginée des défauts d'un asset.
struct Fetched {
    items: Vec<Value>,
    /// Métadonnées de la première réponse (total, noms absents, packages sales...).
    meta: Value,
    warnings: Vec<Value>,
    /// Indice de reprise si la lecture s'est arrêtée avant la fin.
    next_index: Option<usize>,
    commands: usize,
}

/// Chemin d'objet normalisé: "/Game/X/Y" devient "/Game/X/Y.Y". Refuse tout ce qui ne ressemble pas
/// à un chemin d'asset Unreal.
fn normalize_object_path(raw: &str) -> Result<String, String> {
    let path = raw.trim();
    let bad = |why: &str| {
        Err(format!(
            "ERREUR InvalidParameter: object_path {raw:?} refusé ({why}). Attendu: un chemin d'objet Unreal \
             comme \"/Game/Mods/test/test/Mailbox/BP_PL_Mailbox.BP_PL_Mailbox\" (voir ue_search_assets)."
        ))
    };
    if path.is_empty() || path.chars().count() > UE_MAX_TEXT {
        return bad("vide ou trop long");
    }
    if !path.starts_with('/') {
        return bad("doit commencer par '/'");
    }
    if path.chars().any(|c| {
        c.is_control()
            || c.is_whitespace()
            || matches!(c, '"' | '\'' | '\\' | '<' | '>' | '|' | '?' | '*')
    }) {
        return bad("caractère interdit");
    }
    if path.contains("..") || path.ends_with('/') {
        return bad("chemin invalide");
    }
    if path
        .rsplit('/')
        .next()
        .is_some_and(|last| last.contains('.'))
    {
        return Ok(path.to_owned());
    }
    let last = path.rsplit('/').next().unwrap_or(path);
    Ok(format!("{path}.{last}"))
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
            ue: Arc::new(BridgeConfig::default_multicast()),
            ue_lock: Arc::new(tokio::sync::Mutex::new(())),
            ue_suspect: Arc::new(std::sync::Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }

    /// Remplace la configuration du pont vers le Devkit (projet visé, délais, découverte).
    pub fn with_bridge(mut self, bridge: BridgeConfig) -> Self {
        self.ue = Arc::new(bridge);
        self
    }

    /// Exécute un gabarit figé dans l'éditeur: une session, hors du runtime async.
    async fn ue_template(
        &self,
        template: templates::Template,
        params: Value,
    ) -> Result<TemplateOutput, UeError> {
        let _session = self.ue_lock.lock().await;
        let is_status = template.name == templates::STATUS.name;
        if !is_status {
            if let Some(why) = self.ue_suspect_reason() {
                return Err(UeError::EditorBusy(format!(
                    "l'éditeur est peut-être encore occupé: une commande précédente n'a pas abouti ({why}). \
                     Rien n'a été envoyé. NE RELANCE PAS d'outil ue_* et préviens Robin pour qu'il regarde \
                     le Devkit (Python peut tourner encore; au-delà de 10 s l'éditeur peut planter). Seul \
                     ue_status peut lever ce blocage, une fois l'éditeur redevenu disponible."
                )));
            }
        }
        let cfg = self.ue.clone();
        let result =
            tokio::task::spawn_blocking(move || ue_bridge::run_template(&cfg, &template, &params))
                .await
                .map_err(|e| UeError::ProtocolError(format!("tâche du pont interrompue: {e}")))?;
        match &result {
            Err(e) if e.command_in_doubt() => self.set_ue_suspect(Some(e.to_string())),
            Ok(_) if is_status => self.set_ue_suspect(None),
            _ => {}
        }
        result
    }

    fn ue_suspect_reason(&self) -> Option<String> {
        self.ue_suspect
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_ue_suspect(&self, reason: Option<String>) {
        *self.ue_suspect.lock().unwrap_or_else(|e| e.into_inner()) = reason;
    }

    /// Lit les défauts d'un asset commande par commande (60 éléments au plus et 2 s au plus chacune),
    /// en suivant `next_index`, avec au plus `UE_MAX_COMMANDS` commandes.
    async fn ue_fetch_defaults(&self, base: &Value, start: usize) -> Result<Fetched, String> {
        let mut start = start;
        let mut items: Vec<Value> = Vec::new();
        let mut meta = Value::Null;
        let mut warnings: Vec<Value> = Vec::new();
        let mut commands = 0usize;
        loop {
            let mut params = base.clone();
            params["start"] = json!(start);
            let out = self
                .ue_template(templates::DEFAULTS, params)
                .await
                .map_err(|e| ue_error_text(&e))?;
            commands += 1;
            // une erreur applicative du script (asset introuvable, pas un Blueprint...) est déjà devenue
            // une erreur d'outil dans `run_template`
            let payload = out.payload;
            if let Some(list) = payload["items"].as_array() {
                items.extend(list.iter().cloned());
            }
            if let Some(list) = payload["warnings"].as_array() {
                warnings.extend(list.iter().cloned());
            }
            if meta.is_null() {
                meta = json!({
                    "object_path": payload["object_path"],
                    "section": payload["section"],
                    "total": payload["total"],
                    "missing_names": payload["missing_names"],
                });
            }
            meta["dirty_packages"] = payload["dirty_packages"].clone();
            let next = payload["next_index"].as_u64().map(|n| n as usize);
            match next {
                None => {
                    return Ok(Fetched {
                        items,
                        meta,
                        warnings,
                        next_index: None,
                        commands,
                    })
                }
                Some(n) if n <= start => {
                    return Err(format!(
                        "ERREUR ProtocolError: la lecture des défauts ne progresse pas (next_index={n}, \
                         start={start}); abandon, rien n'a été modifié."
                    ))
                }
                Some(n) => start = n,
            }
            if commands >= UE_MAX_COMMANDS {
                return Ok(Fetched {
                    items,
                    meta,
                    warnings,
                    next_index: Some(start),
                    commands,
                });
            }
        }
    }

    /// Mode « valeurs proposées » de ue_diff_defaults: lit seulement les variables nommées dans `proposed`
    /// (section members) et classe chaque valeur proposée. Ne modifie rien.
    async fn ue_diff_proposed(
        &self,
        p: &UeDiffParams,
        proposed: &serde_json::Map<String, Value>,
    ) -> Result<CallToolResult, String> {
        if proposed.is_empty() {
            return Err(
                "ERREUR InvalidParameter: `proposed` est vide; attendu {nom exact: valeur}.".into(),
            );
        }
        if p.section.as_deref().is_some_and(|s| s.trim() != "members")
            || p.component.is_some()
            || p.names.is_some()
            || p.expand.is_some()
        {
            return Err(
                "ERREUR InvalidParameter: avec `proposed`, seules les variables membres sont \
                        comparées; `section`, `component`, `names` et `expand` ne s'appliquent pas \
                        (les noms sont les clés de `proposed`)."
                    .into(),
            );
        }
        if let Some((name, _)) = proposed
            .iter()
            .find(|(_, v)| v.to_string().chars().count() > UE_MAX_TEXT * 4)
        {
            return Err(format!(
                "ERREUR InvalidParameter: la valeur proposée pour {name:?} est trop longue."
            ));
        }
        let names: Vec<String> = proposed.keys().cloned().collect();
        let base = ue_defaults_base(&p.object_path_a, &None, &None, &Some(names), &None)?;
        let f = self.ue_fetch_defaults(&base, 0).await?;
        if f.next_index.is_some() {
            return Err(format!(
                "ERREUR TooLarge: lecture incomplète après {UE_MAX_COMMANDS} commandes; réduire `proposed`."
            ));
        }
        let missing: Vec<String> = f.meta["missing_names"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        let mut result = ue_defaults::diff_proposed(&f.items, &missing, proposed);
        result["object_path"] = base["object_path"].clone();
        result["commands"] = json!(f.commands);
        result["dirty_packages"] = f.meta["dirty_packages"].clone();
        result["warnings"] = json!(f.warnings);
        Ok(CallToolResult::structured(without_nulls(result)))
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

    /// Exécute une opération par ligne (add_row / update_row / delete_row) pour `table`.
    fn row_operation(
        &self,
        tool: &str,
        table: &str,
        path: Option<String>,
        op: RowOp,
        dry_run: Option<bool>,
        confirm: Option<bool>,
    ) -> Result<CallToolResult, String> {
        let dry_run = dry_run.unwrap_or(true);
        let confirm = confirm.unwrap_or(false);
        if !dry_run && !confirm {
            return Err(format!(
                "ERREUR ConfirmRequired: {tool} refusé pour une écriture réelle. Il faut dry_run=false ET \
                 confirm=true, explicitement. Rien n'a été écrit; relancer d'abord en dry_run (défaut) \
                 pour voir le diff."
            ));
        }
        // Pas de schéma valide, pas d'opération: même en dry-run.
        let schema = self.schema_for(table)?;
        let path = path.unwrap_or_else(|| format!("{}.csv", schema.name));
        let key = op.key().to_owned();
        match self
            .store
            .apply_row_op(&schema, &path, &op, WriteOptions { dry_run, confirm })
        {
            Ok(row) if row.no_change => {
                let text = format!(
                    "AUCUN CHANGEMENT: {tool} sur la clé {} de la table {} ne change aucune valeur. Rien n'a \
                     été écrit, aucune sauvegarde créée.\n",
                    row.key, schema.name
                );
                let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
                result.structured_content = Some(json!({
                    "table": schema.name, "tool": tool, "key": row.key, "path": path,
                    "no_change": true, "applied": false, "dry_run": false,
                }));
                Ok(result)
            }
            Ok(row) => {
                let label = format!("{tool} — clé {}", row.key);
                Ok(self.write_result(&schema, &path, row.write, Some(&label)))
            }
            Err(StoreError::ValidationFailed { table, errors }) => {
                Ok(validation_refused(&table, &errors))
            }
            Err(e @ StoreError::Sandbox(conan_core::SandboxError::NotFound { .. })) => Err(format!(
                "{} Le CSV de la table doit déjà exister (path={path:?}); pour le créer, utiliser write_datatable. \
                 Opération sur la clé {key:?}.",
                error_text(&e)
            )),
            Err(e) => Err(error_text(&e)),
        }
    }

    /// Rend le résultat d'une écriture (dry-run ou réelle) validée par schéma.
    fn write_result(
        &self,
        schema: &Schema,
        path: &str,
        written: SchemaWrite,
        operation: Option<&str>,
    ) -> CallToolResult {
        let (outcome, validation, normalized) =
            (written.outcome, written.report, written.normalized);
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
        let mut report = format!("{status}\n");
        if let Some(op) = operation {
            report.push_str(&format!("Opération: {op}\n"));
        }
        report.push_str(&format!(
            "Table: {} — fichier: {} ({})\nChemin réel: {}\n{}",
            schema.name,
            path,
            if outcome.existed {
                "existant"
            } else {
                "nouveau"
            },
            pretty(&outcome.path),
            outcome.diff.render(),
        ));
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
        if normalized {
            report.push_str(
                "Format: le contenu reçu a été remis au format du Devkit (en-tête et clés sans guillemets, \
                 valeurs entre guillemets, flottants à six décimales, LF) avant comparaison et écriture; \
                 le diff ne montre que les changements de valeur.\n",
            );
        }
        if let (Some(b), Some(rel)) = (&outcome.backup, &backup_rel) {
            report.push_str(&format!(
                "Sauvegarde créée: {rel}\nChemin réel de la sauvegarde: {}\n",
                pretty(b)
            ));
        }
        if !outcome.applied && !outcome.unchanged && validation.is_empty() {
            report
                .push_str("Pour écrire réellement: rappeler avec dry_run=false et confirm=true.\n");
        }
        let mut result = CallToolResult::success(vec![ContentBlock::text(report)]);
        let mut structured = structured_write(path, &outcome, backup_rel);
        structured["table"] = json!(schema.name);
        structured["valid"] = json!(validation.is_empty());
        structured["normalized"] = json!(normalized);
        structured["schema_drift"] = json!(validation.iter().any(|e| e.rule.is_schema_drift()));
        structured["validation_errors"] =
            json!(validation.iter().map(error_json).collect::<Vec<_>>());
        result.structured_content = Some(structured);
        result
    }

    /// Charge le schéma d'une table (erreur bloquante s'il est absent ou malformé).
    fn schema_for(&self, table: &str) -> Result<Schema, String> {
        self.schema_store()?
            .load(table)
            .map_err(|e| format!("ERREUR {}: {e}", e.code()))
    }
}

/// Écriture réelle refusée par la validation: erreur d'outil avec le rapport complet.
fn validation_refused(table: &str, errors: &[ValidationError]) -> CallToolResult {
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
        if drift {
            "dérive de schéma: l'en-tête ne correspond pas au schéma"
        } else {
            "valeurs invalides"
        },
        report_text(errors),
    );
    let mut result = CallToolResult::error(vec![ContentBlock::text(text)]);
    result.structured_content = Some(json!({
        "table": table, "valid": false, "schema_drift": drift,
        "error_count": errors.len(),
        "errors": errors.iter().map(error_json).collect::<Vec<_>>(),
    }));
    result
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
            match self.store.write_with_schema(
                &schema,
                &path,
                p.content.as_bytes(),
                WriteOptions { dry_run, confirm },
            ) {
                Ok(written) => Ok(self.write_result(&schema, &path, written, None)),
                Err(StoreError::ValidationFailed { table, errors }) => {
                    Ok(validation_refused(&table, &errors))
                }
                Err(e) => Err(error_text(&e)),
            }
        })
    }

    #[tool(
        description = "AJOUTE une ligne à la FIN d'une DataTable existante (CSV sous CSV_ROOT), validée contre \
                       le schéma de la table. La clé (`key`, nom de ligne) ne doit pas exister déjà, casse \
                       ignorée (noms de ligne Unreal); sa casse est conservée. `values` doit contenir TOUTES \
                       les colonnes du schéma (voir describe_schema); toute colonne inconnue est refusée. Le \
                       CSV existant doit être valide (sinon refus, utiliser validate_rows: aucune \
                       réparation) et la table entière résultante est revalidée. PAR DÉFAUT dry_run=true: \
                       aperçu (diff + validation), rien n'est écrit. Écriture réelle: dry_run=false ET \
                       confirm=true; le CSV existant est d'abord sauvegardé dans .backup. Pour modifier une \
                       ligne existante, utiliser update_row; pour la supprimer, delete_row."
    )]
    fn add_row(&self, Parameters(p): Parameters<AddRowParams>) -> Result<CallToolResult, String> {
        guarded(|| {
            let values = values_to_text(&p.values)?;
            self.row_operation(
                "add_row",
                &p.table,
                p.path.clone(),
                RowOp::Add {
                    key: p.key.clone(),
                    values,
                },
                p.dry_run,
                p.confirm,
            )
        })
    }

    #[tool(
        description = "MODIFIE une ligne existante d'une DataTable (CSV sous CSV_ROOT), validée contre le \
                       schéma de la table. La clé (`key`) doit exister, casse ignorée (la casse stockée est \
                       conservée). `values` ne contient QUE les colonnes à changer: elles sont fusionnées avec \
                       la ligne existante, les autres colonnes sont inchangées. La colonne clé n'est PAS \
                       modifiable (renommer = delete_row puis add_row). Si la fusion ne change aucune valeur, \
                       la réponse est « AUCUN CHANGEMENT » sans écriture ni sauvegarde. Le CSV existant doit \
                       être valide (sinon refus, utiliser validate_rows: aucune réparation) et la table \
                       entière résultante est revalidée. PAR DÉFAUT dry_run=true: aperçu (diff + validation). \
                       Écriture réelle: dry_run=false ET confirm=true; sauvegarde dans .backup avant écriture."
    )]
    fn update_row(
        &self,
        Parameters(p): Parameters<UpdateRowParams>,
    ) -> Result<CallToolResult, String> {
        guarded(|| {
            let values = values_to_text(&p.values)?;
            self.row_operation(
                "update_row",
                &p.table,
                p.path.clone(),
                RowOp::Update {
                    key: p.key.clone(),
                    values,
                },
                p.dry_run,
                p.confirm,
            )
        })
    }

    #[tool(
        description = "SUPPRIME DÉFINITIVEMENT une ligne d'une DataTable existante (CSV sous CSV_ROOT; une \
                       sauvegarde de l'état précédent est faite dans .backup). La clé (`key`) doit exister, \
                       casse ignorée. PAR DÉFAUT dry_run=true: le diff montre la ligne qui SERAIT supprimée, \
                       rien n'est écrit — présentez-la à l'utilisateur avant de continuer. Suppression \
                       réelle: dry_run=false ET confirm=true, explicites. Le CSV existant doit être valide \
                       (sinon refus, utiliser validate_rows: aucune réparation) et la table résultante est \
                       revalidée. L'ordre des autres lignes est conservé."
    )]
    fn delete_row(
        &self,
        Parameters(p): Parameters<DeleteRowParams>,
    ) -> Result<CallToolResult, String> {
        guarded(|| {
            self.row_operation(
                "delete_row",
                &p.table,
                p.path.clone(),
                RowOp::Delete { key: p.key.clone() },
                p.dry_run,
                p.confirm,
            )
        })
    }

    #[tool(
        description = "LECTURE SEULE. Indique si le Devkit Unreal (éditeur ouvert, Python Remote Execution \
                       activé) est joignable et renvoie la version du moteur, le projet et le nombre d'assets \
                       du registre. Si le Devkit est fermé, la réponse dit reachable=false (le démarrage de \
                       l'éditeur prend environ 2 minutes). À appeler avant les autres outils ue_*. Après une commande \
                       expirée, les autres outils ue_* répondent EditorBusy jusqu'à un ue_status réussi."
    )]
    async fn ue_status(&self) -> Result<CallToolResult, String> {
        const BUSY: &str = "éditeur peut-être occupé (commande précédente expirée)";
        let was_suspect = self.ue_suspect_reason().is_some();
        match self.ue_template(templates::STATUS, json!({})).await {
            Ok(out) => {
                let mut status = out.payload;
                if let Some(obj) = status.as_object_mut() {
                    obj.insert("reachable".into(), json!(true));
                    obj.insert("node".into(), out.node.data);
                    if was_suspect {
                        obj.insert("busy_flag_cleared".into(), json!(true));
                    }
                }
                Ok(CallToolResult::structured(status))
            }
            Err(UeError::DevkitNotRunning(detail)) => {
                let mut r = json!({
                    "reachable": false,
                    "error_code": "DevkitNotRunning",
                    "error": detail,
                });
                if self.ue_suspect_reason().is_some() {
                    r["busy_suspected"] = json!(true);
                    r["note"] = json!(BUSY);
                }
                Ok(CallToolResult::structured(r))
            }
            Err(e) if self.ue_suspect_reason().is_some() => {
                Err(format!("{} — {BUSY}", ue_error_text(&e)))
            }
            Err(e) => Err(ue_error_text(&e)),
        }
    }

    #[tool(
        description = "LECTURE SEULE. Cherche des assets dans l'Asset Registry du Devkit Unreal (le contenu \
                       de base du jeu, ~85 000 assets, n'est pas visible par fichiers). Filtres facultatifs: \
                       path_prefix (ex. \"Mods/test\"), class_name (ex. \"DataTable\", \"Blueprint\"), \
                       name_contains. limit: 50 par défaut, 500 maximum. Renvoie, pour chaque asset, \
                       object_path, classe et package, plus le nombre total trouvé; résultats triés par \
                       object_path. N'ouvre aucun asset. \
                       Nécessite le Devkit ouvert (voir ue_status)."
    )]
    async fn ue_search_assets(
        &self,
        Parameters(p): Parameters<UeSearchParams>,
    ) -> Result<CallToolResult, String> {
        let prefix = ue_text_param("path_prefix", &p.path_prefix)?;
        let class = ue_text_param("class_name", &p.class_name)?;
        let contains = ue_text_param("name_contains", &p.name_contains)?;
        let requested = p.limit.unwrap_or(UE_DEFAULT_LIMIT);
        if requested == 0 {
            return Err("ERREUR InvalidParameter: limit doit être au moins 1.".to_owned());
        }
        let limit = requested.min(UE_MAX_LIMIT);
        let out = self
            .ue_template(
                templates::SEARCH_ASSETS,
                json!({"path_prefix": prefix, "class_name": class, "name_contains": contains, "limit": limit}),
            )
            .await
            .map_err(|e| ue_error_text(&e))?;
        let payload = out.payload;
        let total = payload["total"].as_u64().unwrap_or(0);
        let assets = payload["assets"].as_array().cloned().unwrap_or_default();
        Ok(CallToolResult::structured(json!({
            "total": total,
            "returned": assets.len(),
            "limit": limit,
            "limit_clamped": requested > UE_MAX_LIMIT,
            "truncated": total as usize > assets.len(),
            "assets": assets,
            "warnings": payload["warnings"],
        })))
    }

    #[tool(
        description = "LECTURE SEULE. Décrit un asset du Devkit Unreal à partir de l'Asset Registry, SANS le \
                       charger: classe, package, tags du registre (liste de tags courants: RowStructure pour \
                       une DataTable, ParentClass pour un Blueprint, Triangles/Vertices pour un mesh...; \
                       l'API Python ne permet pas d'énumérer tous les tags) et dépendances directes. \
                       object_path vient de ue_search_assets. Nécessite le Devkit ouvert."
    )]
    async fn ue_describe_asset(
        &self,
        Parameters(p): Parameters<UeAssetParams>,
    ) -> Result<CallToolResult, String> {
        let object_path = normalize_object_path(&p.object_path)?;
        let out = self
            .ue_template(
                templates::DESCRIBE_ASSET,
                json!({"object_path": object_path}),
            )
            .await
            .map_err(|e| ue_error_text(&e))?;
        Ok(CallToolResult::structured(without_nulls(out.payload)))
    }

    #[tool(
        description = "LECTURE SEULE. Liste les variables déclarées dans un Blueprint du Devkit Unreal: nom, \
                       type (int, bool, real(double), name, text, object<Classe>, Array of ...), catégorie, \
                       réplication et valeur par défaut lue sur le CDO. Par défaut seulement les variables \
                       propres au Blueprint (include_inherited=true ajoute celles des classes parentes). \
                       Charge le Blueprint indiqué (et lui seul) en mémoire sans le modifier. Limites: la \
                       valeur par défaut d'une variable objet est un chemin d'asset, les flottants simple \
                       précision apparaissent en double. Lecture bornée: au plus 60 variables et 2 s par \
                       appel (suite avec `start` = `next_index`); tableaux, maps et structs ne sont pas \
                       dépliés par défaut (`expand` pour 20 éléments au plus). Nécessite le Devkit ouvert."
    )]
    async fn ue_blueprint_variables(
        &self,
        Parameters(p): Parameters<UeBlueprintParams>,
    ) -> Result<CallToolResult, String> {
        let object_path = normalize_object_path(&p.object_path)?;
        let expand = ue_names_param("expand", &p.expand, UE_MAX_EXPAND)?.unwrap_or_default();
        let out = self
            .ue_template(
                templates::BLUEPRINT_VARIABLES,
                json!({
                    "object_path": object_path,
                    "include_inherited": p.include_inherited.unwrap_or(false),
                    "start": p.start.unwrap_or(0),
                    "expand": expand,
                }),
            )
            .await
            .map_err(|e| ue_error_text(&e))?;
        Ok(CallToolResult::structured(without_nulls(out.payload)))
    }

    #[tool(
        description = "LECTURE SEULE. Lit les valeurs par défaut d'un Blueprint du Devkit Unreal: section \
                       \"members\" (défaut: toutes les propriétés, héritées comprises, lues sur le CDO, avec \
                       type, classe propriétaire et valeur), \"components\" (composants, hérités compris) ou \
                       \"component_values\" (propriétés d'un composant). Noms de propriétés EXACTS (comme dans \
                       `names`), pas les noms affichés. Lecture bornée: 60 éléments et 2 s par commande, suite \
                       automatique jusqu'à 12 commandes; `complete=false` + `next_index` si le Blueprint est \
                       plus grand (rappeler avec `start`). Tableaux, maps et structs ne sont PAS dépliés \
                       (longueur seulement); `expand` en déplie 20 éléments au plus. Une propriété illisible \
                       est listée avec sa raison. Charge le Blueprint indiqué sans le modifier. Nécessite le \
                       Devkit ouvert."
    )]
    async fn ue_get_defaults(
        &self,
        Parameters(p): Parameters<UeDefaultsParams>,
    ) -> Result<CallToolResult, String> {
        let base = ue_defaults_base(
            &p.object_path,
            &p.section,
            &p.component,
            &p.names,
            &p.expand,
        )?;
        let f = self.ue_fetch_defaults(&base, p.start.unwrap_or(0)).await?;
        Ok(CallToolResult::structured(without_nulls(json!({
            "object_path": f.meta["object_path"],
            "section": f.meta["section"],
            "total": f.meta["total"],
            "start": p.start.unwrap_or(0),
            "returned": f.items.len(),
            "complete": f.next_index.is_none(),
            "next_index": f.next_index,
            "commands": f.commands,
            "missing_names": f.meta["missing_names"],
            "dirty_packages": f.meta["dirty_packages"],
            "warnings": f.warnings,
            "items": f.items,
        }))))
    }

    #[tool(
        description = "LECTURE SEULE (rien n'est écrit). Deux modes. (1) `object_path_b`: compare les valeurs \
                       par défaut de DEUX Blueprints (mêmes sections que ue_get_defaults); renvoie les \
                       différences (valeur A, valeur B, type), les propriétés présentes d'un seul côté et celles \
                       NON comparées (illisibles ou conteneurs/structs non dépliés: `expand`); flottants \
                       comparés exactement quand la précision est connue (real(float) ou real(double)), \
                       sinon à 1e-6 près; entiers exacts; si un Blueprint dépasse 12 commandes, erreur: restreindre \
                       avec `names`. (2) `proposed` = {nom exact: valeur}: compare des valeurs PROPOSÉES aux \
                       valeurs actuelles de `object_path_a` (dry-run d'une future écriture). Statut par \
                       variable: unchanged, modified, type_mismatch, unknown_variable (nom inconnu, casse et \
                       espaces comptent), not_comparable (illisible, conteneur, struct); `valid` est vrai si \
                       tout est comparable et de bon type. real(float): la valeur proposée est arrondie en \
                       float 32 bits (`proposed_stored`), avec `not_representable`+`warning` si l'arrondi \
                       change les chiffres saisis (16777217 → 16777216); real(double): comparaison exacte. Exactement un des deux paramètres. Nécessite le \
                       Devkit ouvert."
    )]
    async fn ue_diff_defaults(
        &self,
        Parameters(p): Parameters<UeDiffParams>,
    ) -> Result<CallToolResult, String> {
        match (&p.object_path_b, &p.proposed) {
            (Some(_), Some(_)) => {
                return Err(
                    "ERREUR InvalidParameter: `object_path_b` et `proposed` sont exclusifs; \
                            fournir l'un ou l'autre."
                        .into(),
                )
            }
            (None, None) => {
                return Err(
                    "ERREUR InvalidParameter: fournir `object_path_b` (comparer deux Blueprints) \
                            ou `proposed` (comparer des valeurs proposées)."
                        .into(),
                )
            }
            (None, Some(proposed)) => return self.ue_diff_proposed(&p, proposed).await,
            (Some(_), None) => {}
        }
        let object_path_b = p.object_path_b.clone().unwrap_or_default();
        let base_a = ue_defaults_base(
            &p.object_path_a,
            &p.section,
            &p.component,
            &p.names,
            &p.expand,
        )?;
        let base_b = ue_defaults_base(
            &object_path_b,
            &p.section,
            &p.component,
            &p.names,
            &p.expand,
        )?;
        let mut fetched = Vec::new();
        for base in [&base_a, &base_b] {
            let f = self.ue_fetch_defaults(base, 0).await?;
            if f.next_index.is_some() {
                return Err(format!(
                    "ERREUR TooLarge: {} compte plus de {} éléments à lire (limite de {UE_MAX_COMMANDS} \
                     commandes). Restreindre la comparaison avec `names`.",
                    base["object_path"].as_str().unwrap_or("?"),
                    f.items.len()
                ));
            }
            fetched.push(f);
        }
        let (a, b) = (&fetched[0], &fetched[1]);
        let mut result = ue_defaults::diff_items(&a.items, &b.items);
        result["a"] = json!({"object_path": base_a["object_path"], "total": a.meta["total"], "missing_names": a.meta["missing_names"]});
        result["b"] = json!({"object_path": base_b["object_path"], "total": b.meta["total"], "missing_names": b.meta["missing_names"]});
        result["section"] = base_a["section"].clone();
        result["complete"] = json!(true);
        result["commands"] = json!(a.commands + b.commands);
        let mut dirty: Vec<Value> = Vec::new();
        for f in [a, b] {
            for d in f.meta["dirty_packages"].as_array().into_iter().flatten() {
                if !dirty.contains(d) {
                    dirty.push(d.clone());
                }
            }
        }
        result["dirty_packages"] = json!(dirty);
        result["warnings"] = json!([a.warnings.clone(), b.warnings.clone()].concat());
        Ok(CallToolResult::structured(without_nulls(result)))
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
                 confirm=true. Les outils ue_* interrogent le Devkit Unreal ouvert, en lecture seule. \
                 Pour agir sur une seule ligne, préférez add_row, update_row ou \
                 delete_row à write_datatable (qui remplace le fichier entier). L'import CSV -> DataTable reste manuel dans le Devkit.",
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
    let config = Config::from_vars(&get).context("configuration invalide")?;
    Ok(build_server(&config)?.with_bridge(BridgeConfig::from_vars(&get)))
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
