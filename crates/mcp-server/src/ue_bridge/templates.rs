//! Gabarits Python figés, exécutés dans l'éditeur. **Lecture seule.**
//!
//! Règles (ordre de mission 4a):
//! - le code Python est une constante Rust; les paramètres y entrent uniquement par
//!   `json.loads(<littéral>)` où le littéral est le JSON des paramètres sérialisé côté Rust,
//!   jamais par interpolation dans le code;
//! - le script imprime son résultat en JSON, sur UNE ligne, entre deux marqueurs;
//! - interdits dans les gabarits: `save_*`, `delete_*`, `rename_*`, `duplicate_*`,
//!   `set_editor_property`, tout import d'asset (`import_asset*`, tâches d'import), toute écriture
//!   disque ou exécution dynamique. Ce n'est pas l'instruction Python `import json` / `import unreal`
//!   qui est visée. `load_asset` ne sert qu'à inspecter un asset ciblé, jamais en boucle.
//!   Un test (`forbidden_calls_never_appear_in_templates`) l'impose.

use serde_json::Value;

pub const MARKER_BEGIN: &str = "<<CONAN_JSON_BEGIN>>";
pub const MARKER_END: &str = "<<CONAN_JSON_END>>";
const PARAMS_TOKEN: &str = "__CONAN_PARAMS__";

/// Début commun à tous les gabarits (décodage des paramètres, émission du résultat).
const PRELUDE: &str = r##"
import json
import re
import sys
import time
import unreal

_PARAMS = json.loads(__CONAN_PARAMS__)
_BEGIN = "<<CONAN_JSON_BEGIN>>"
_END = "<<CONAN_JSON_END>>"


def _emit(payload):
    print(_BEGIN + json.dumps(payload, ensure_ascii=False, default=str) + _END)


def _try(warnings, label, fn, default=None):
    try:
        return fn()
    except Exception as exc:
        warnings.append(label + ": " + type(exc).__name__ + ": " + str(exc))
        return default


def _class_of(asset_data):
    path = getattr(asset_data, "asset_class_path", None)
    if path is not None:
        return str(path.asset_name), str(path.package_name)
    return str(getattr(asset_data, "asset_class", "")), ""

"##;

const STATUS_BODY: &str = r##"
def _main():
    warnings = []
    registry = unreal.AssetRegistryHelpers.get_asset_registry()
    info = {"warnings": warnings}
    info["engine_version"] = _try(warnings, "engine_version", lambda: str(unreal.SystemLibrary.get_engine_version()))
    info["project_name"] = _try(warnings, "project_name", lambda: str(unreal.SystemLibrary.get_game_name()))
    info["project_dir"] = _try(warnings, "project_dir", lambda: str(unreal.SystemLibrary.get_project_directory()))
    info["registry_loading"] = _try(warnings, "registry_loading", lambda: bool(registry.is_loading_assets()))
    info["asset_count"] = _try(warnings, "asset_count", lambda: len(registry.get_all_assets()))
    info["python_version"] = sys.version.split()[0]
    _emit(info)


_main()
"##;

const SEARCH_BODY: &str = r##"
def _prefixes(prefix):
    prefix = (prefix or "").strip().rstrip("/")
    if not prefix:
        return []
    if not prefix.startswith("/"):
        prefix = "/" + prefix
    found = [prefix]
    if not prefix.startswith("/Game"):
        found.append("/Game" + prefix)
    return found


def _main():
    warnings = []
    limit = int(_PARAMS.get("limit", 50))
    wanted_class = (_PARAMS.get("class_name") or "").lower()
    contains = (_PARAMS.get("name_contains") or "").lower()
    registry = unreal.AssetRegistryHelpers.get_asset_registry()
    prefixes = _prefixes(_PARAMS.get("path_prefix"))
    if prefixes:
        assets = []
        seen = set()
        for prefix in prefixes:
            registry_filter = unreal.ARFilter(package_paths=[prefix], recursive_paths=True)
            for asset in registry.get_assets(registry_filter):
                key = str(asset.package_name) + "." + str(asset.asset_name)
                if key not in seen:
                    seen.add(key)
                    assets.append(asset)
    else:
        assets = registry.get_all_assets()
    matches = []
    for asset in assets:
        name = str(asset.asset_name)
        if contains and contains not in name.lower():
            continue
        class_name, class_package = _class_of(asset)
        if wanted_class and class_name.lower() != wanted_class:
            continue
        package = str(asset.package_name)
        matches.append((package + "." + name, package, name, class_name, class_package))
    # Tri par object_path AVANT la limite: le résultat est déterministe.
    matches.sort(key=lambda m: m[0])
    total = len(matches)
    found = []
    for object_path, package, name, class_name, class_package in matches[:limit]:
        found.append({
            "object_path": object_path,
            "class": class_name,
            "class_package": class_package,
            "package": package,
            "name": name,
        })
    _emit({"total": total, "returned": len(found), "assets": found, "prefixes_tried": prefixes, "query": _PARAMS, "warnings": warnings})


_main()
"##;

const DESCRIBE_BODY: &str = r##"
_KNOWN_TAGS = [
    "RowStructure", "AssetImportData", "ParentClass", "NativeParentClass", "BlueprintType",
    "BlueprintDescription", "BlueprintCategory", "BlueprintDisplayName", "ImplementedInterfaces",
    "NativeComponents", "NumNativeComponents", "NumBlueprintComponents", "NumReplicatedProperties",
    "IsDataOnly", "ClassFlags", "GeneratedClass", "Triangles", "Vertices", "Materials", "LODs",
    "ApproxSize", "UVChannels", "NaniteEnabled", "HasSkeleton", "Skeleton", "Width", "Height",
    "Format", "SourceFile", "Dimensions", "ParentClassPath", "Class", "Duration", "NumFrames",
]


def _main():
    warnings = []
    object_path = _PARAMS["object_path"]
    registry = unreal.AssetRegistryHelpers.get_asset_registry()
    asset = registry.get_asset_by_object_path(object_path)
    valid = asset is not None and (not hasattr(asset, "is_valid") or asset.is_valid())
    if not valid:
        _emit({"error": "Asset introuvable dans l'Asset Registry: " + object_path})
        return
    class_name, class_package = _class_of(asset)
    # L'API Python n'énumère pas les tags du registre (AssetData n'a ni tags_and_values ni accès
    # à la liste complète); on interroge une liste fixe de tags courants via get_tag_value.
    tags = {}
    for tag_name in _KNOWN_TAGS:
        try:
            value = asset.get_tag_value(tag_name)
        except Exception as exc:
            warnings.append("tag " + tag_name + ": " + type(exc).__name__ + ": " + str(exc))
            continue
        if isinstance(value, tuple):
            found, value = value[0], (value[1] if len(value) > 1 else None)
            if not found:
                continue
        if value is not None and str(value) != "":
            tags[tag_name] = str(value)
    dependencies = []

    def _dependencies():
        options = unreal.AssetRegistryDependencyOptions(
            include_soft_package_references=True,
            include_hard_package_references=True,
            include_searchable_names=False,
            include_soft_management_references=False,
            include_hard_management_references=False,
        )
        return registry.get_dependencies(asset.package_name, options)

    raw_dependencies = _try(warnings, "dependencies", _dependencies, [])
    for dep in raw_dependencies or []:
        dependencies.append(str(dep))
    limit = int(_PARAMS.get("max_dependencies", 500))
    _emit({
        "object_path": str(asset.package_name) + "." + str(asset.asset_name),
        "name": str(asset.asset_name),
        "package": str(asset.package_name),
        "package_path": str(getattr(asset, "package_path", "")),
        "class": class_name,
        "class_package": class_package,
        "tags": tags,
        "dependency_count": len(dependencies),
        "dependencies": dependencies[:limit],
        "asset_members": [n for n in dir(asset) if not n.startswith("_")] if _PARAMS.get("diagnostics") else None,
        "warnings": warnings,
    })


_main()
"##;

/// Variables d'un Blueprint: noms, types et catégories via les méthodes de lecture de l'objet
/// `Blueprint` (`list_member_variable_names`, `get_member_variable_type`,
/// `get_blueprint_variable_category`), valeurs par défaut via le CDO de la classe générée.
/// Voir les limites dans docs/phase-4a-test.md.
const BLUEPRINT_BODY: &str = r##"
def _plain(value, depth=0):
    if value is None or isinstance(value, (bool, int, str)):
        return value
    if isinstance(value, float):
        return value if value == value and value not in (float("inf"), float("-inf")) else str(value)
    if hasattr(value, "name") and hasattr(value, "value") and not callable(value.name):
        return type(value).__name__ + "." + str(value.name)
    if hasattr(value, "get_path_name"):
        return value.get_path_name()
    kind = type(value).__name__
    if depth < 2 and (isinstance(value, (list, tuple)) or kind in ("Array", "Set")):
        return [_plain(item, depth + 1) for item in list(value)[:50]]
    if depth < 2 and kind == "Map":
        return {str(key): _plain(value[key], depth + 1) for key in list(value)[:50]}
    return str(value)


def _pin_type(pin_type):
    # Les champs d'EdGraphPinType ne sont pas lisibles un par un; export_text() les donne tous.
    text = pin_type.export_text()
    category = re.search(r'PinCategory="([^"]*)"', text)
    sub = re.search(r'PinSubCategory="([^"]*)"', text)
    obj = re.search(r'PinSubCategoryObject=(?:"([^"]*)"|None)', text)
    container = re.search(r'ContainerType=(\w+)', text)
    category = category.group(1) if category else ""
    sub = sub.group(1) if sub else ""
    obj_path = obj.group(1) if obj and obj.group(1) else None
    if obj_path and "'" in obj_path:
        obj_path = obj_path.split("'")[1]
    container = container.group(1) if container else "None"
    label = category
    if sub:
        label += "(" + sub + ")"
    if obj_path:
        label += "<" + obj_path.rsplit(".", 1)[-1] + ">"
    if container not in ("None", ""):
        label = container + " of " + label
    return {"category": category, "sub_category": sub, "sub_category_object": obj_path, "container": container, "label": label}


def _main():
    warnings = []
    object_path = _PARAMS["object_path"]
    include_inherited = bool(_PARAMS.get("include_inherited", False))
    max_variables = int(_PARAMS.get("max_variables", 500))
    blueprint = unreal.load_asset(object_path)
    if blueprint is None:
        _emit({"error": "Asset introuvable ou non chargeable: " + object_path})
        return
    result = {"object_path": object_path, "asset_python_type": type(blueprint).__name__, "warnings": warnings}
    if not hasattr(blueprint, "list_member_variable_names"):
        _emit({"error": "Cet asset n'est pas un Blueprint dont les variables sont lisibles (type Python " + type(blueprint).__name__ + ")"})
        return
    generated = _try(warnings, "generated_class", lambda: blueprint.generated_class())
    default_object = _try(warnings, "get_default_object", lambda: unreal.get_default_object(generated)) if generated is not None else None
    names = _try(warnings, "list_member_variable_names", lambda: [str(n) for n in blueprint.list_member_variable_names()], [])
    # Les variables héritées sont rapportées avec un nom qualifié par leur classe d'origine
    # ("/Game/X.X_C.Nom" ou "/Script/Module.Classe.Nom"); les variables propres au Blueprint ont un nom simple.
    own_names = [n for n in names if not n.startswith("/")]
    inherited_names = [n for n in names if n.startswith("/")]
    variables = []
    for name in own_names + (inherited_names if include_inherited else []):
        if len(variables) >= max_variables:
            warnings.append("liste tronquée à " + str(max_variables) + " variables")
            break
        entry = {"name": name, "inherited": name.startswith("/")}
        pin = _try(warnings, "type:" + name, lambda: blueprint.get_member_variable_type(name))
        entry["type"] = _pin_type(pin) if pin is not None else None
        entry["category"] = _try(warnings, "category:" + name, lambda: _plain(blueprint.get_blueprint_variable_category(name)))
        entry["replication"] = _try(warnings, "replication:" + name, lambda: _plain(blueprint.get_blueprint_variable_replication(name)))
        entry["default_available"] = False
        entry["default"] = None
        entry["default_python_type"] = None
        if default_object is not None and not name.startswith("/"):
            try:
                value = default_object.get_editor_property(name)
                entry["default"] = _plain(value)
                entry["default_python_type"] = type(value).__name__
                entry["default_available"] = True
            except Exception as exc:
                entry["default_error"] = type(exc).__name__
        variables.append(entry)
    # Vérification (lecture): le chargement ne doit pas avoir marqué le package comme modifié.
    def _dirty():
        package = object_path.split(".")[0]
        dirty = unreal.EditorLoadingAndSavingUtils.get_dirty_content_packages()
        return any(str(p.get_name()) == package for p in dirty)

    result["package_marked_modified"] = _try(warnings, "dirty check", _dirty)
    result["own_variable_count"] = len(own_names)
    result["inherited_variable_count"] = len(inherited_names)
    result["variables"] = variables
    if _PARAMS.get("diagnostics"):
        result["diagnostics"] = {
            "generated_class": str(generated),
            "blueprint_members": [n for n in dir(blueprint) if not n.startswith("_")],
        }
    _emit(result)


_main()
"##;

/// Un gabarit figé: nom et code Python (hors paramètres).
#[derive(Debug, Clone, Copy)]
pub struct Template {
    pub name: &'static str,
    body: &'static str,
}

pub const STATUS: Template = Template {
    name: "status",
    body: STATUS_BODY,
};
pub const SEARCH_ASSETS: Template = Template {
    name: "search_assets",
    body: SEARCH_BODY,
};
pub const DESCRIBE_ASSET: Template = Template {
    name: "describe_asset",
    body: DESCRIBE_BODY,
};
pub const BLUEPRINT_VARIABLES: Template = Template {
    name: "blueprint_variables",
    body: BLUEPRINT_BODY,
};

pub const ALL: [Template; 4] = [STATUS, SEARCH_ASSETS, DESCRIBE_ASSET, BLUEPRINT_VARIABLES];

impl Template {
    pub(crate) const fn new(name: &'static str, body: &'static str) -> Template {
        Template { name, body }
    }

    /// Code Python complet. Les paramètres sont sérialisés en JSON puis re-sérialisés en littéral
    /// de chaîne (un littéral JSON est un littéral Python valide): `json.loads("...")` les relit,
    /// quel que soit leur contenu (guillemets, retours à la ligne, `'''`, jetons...).
    pub fn render(&self, params: &Value) -> String {
        let json_text = serde_json::to_string(params).unwrap_or_else(|_| "null".into());
        let literal = serde_json::to_string(&json_text).unwrap_or_else(|_| "\"null\"".into());
        let source = format!("{PRELUDE}\n{}", self.body);
        source.replacen(PARAMS_TOKEN, &literal, 1)
    }

    /// Le code Python sans paramètres (pour les vérifications statiques).
    pub fn source(&self) -> String {
        format!("{PRELUDE}\n{}", self.body)
    }
}

/// Extrait le JSON imprimé entre les marqueurs (le dernier bloc complet).
pub fn extract_payload(text: &str) -> Result<Value, String> {
    let begin = text
        .rfind(MARKER_BEGIN)
        .ok_or_else(|| "marqueur de début absent de la sortie du script".to_owned())?;
    let after = begin + MARKER_BEGIN.len();
    let end = text[after..]
        .find(MARKER_END)
        .ok_or_else(|| "marqueur de fin absent de la sortie du script".to_owned())?;
    serde_json::from_str(text[after..after + end].trim())
        .map_err(|e| format!("JSON du script illisible: {e}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::process::Command;

    #[test]
    fn extraction_between_markers() {
        let out =
            format!("LogPython: bruit\n{MARKER_BEGIN}{{\"a\": [1, 2]}}{MARKER_END}\nautre ligne");
        assert_eq!(extract_payload(&out).unwrap(), json!({"a": [1, 2]}));
        // préfixe de log sur la ligne, plusieurs blocs: le dernier gagne
        let out = format!("{MARKER_BEGIN}{{\"v\":1}}{MARKER_END}\nLogPython: {MARKER_BEGIN}{{\"v\":2}}{MARKER_END}");
        assert_eq!(extract_payload(&out).unwrap(), json!({"v": 2}));
        // accents et sauts de ligne échappés
        let out = format!("{MARKER_BEGIN}{}{MARKER_END}", json!({"s": "é\n⚔"}));
        assert_eq!(extract_payload(&out).unwrap(), json!({"s": "é\n⚔"}));
    }

    #[test]
    fn extraction_errors_are_explicit() {
        assert!(extract_payload("rien").unwrap_err().contains("début"));
        assert!(extract_payload(&format!("{MARKER_BEGIN}{{"))
            .unwrap_err()
            .contains("fin"));
        assert!(
            extract_payload(&format!("{MARKER_BEGIN}pas json{MARKER_END}"))
                .unwrap_err()
                .contains("illisible")
        );
    }

    #[test]
    fn params_are_embedded_as_a_json_literal_never_as_code() {
        let nasty = json!({
            "name_contains": "a\"b'c\\d\n''' + __import__('os').system('x') + '''\u{2028}é⚔\u{0}",
            "limit": 5,
            "token": PARAMS_TOKEN,
        });
        let script = SEARCH_ASSETS.render(&nasty);
        // le jeton n'apparaît plus qu'à l'intérieur du littéral JSON des paramètres
        assert_eq!(script.matches("json.loads(\"").count(), 1);
        assert!(!script.contains("json.loads(__CONAN_PARAMS__)"));
        // le code (hors littéral) est identique à celui du gabarit
        let literal_line = script
            .lines()
            .find(|l| l.starts_with("_PARAMS = json.loads("))
            .unwrap();
        let rest: String = script.replace(literal_line, "");
        let clean: String = SEARCH_ASSETS
            .source()
            .replace("_PARAMS = json.loads(__CONAN_PARAMS__)", "");
        assert_eq!(rest, clean);
    }

    /// Interdits communs aux gabarits des outils et aux sondes (lecture seule stricte).
    pub(crate) fn assert_read_only(t: &Template) {
        const FORBIDDEN: &[&str] = &[
            "save_",
            "delete_",
            "rename_",
            "duplicate_",
            "set_editor_property",
            "import_asset",
            "AssetImportTask",
            "AssetTools",
            "EditorAssetLibrary",
            "open(",
            ".write(",
            "os.",
            "shutil",
            "subprocess",
            "exec(",
            "eval(",
            "__import__",
            "remove(",
            "unlink",
            "mkdir",
            "makedirs",
            "checkout",
            "mark_package_dirty",
            "modify(",
            "compile_blueprint",
            "request_",
            "add_member_variable",
            "remove_",
            "set_blueprint_variable",
            "change_member_variable_type",
            "reparent_",
            "refresh_",
            "add_function",
            "add_event",
            "replace_variable",
        ];
        let src = t.source();
        for bad in FORBIDDEN {
            assert!(!src.contains(bad), "gabarit {}: contient {bad:?}", t.name);
        }
        // seuls `import json`, `import re`, `import sys`, `import unreal` sont admis (en tête de fichier)
        for line in src.lines().filter(|l| {
            l.trim_start().starts_with("import ") || l.trim_start().starts_with("from ")
        }) {
            assert!(
                [
                    "import json",
                    "import re",
                    "import sys",
                    "import time",
                    "import unreal"
                ]
                .contains(&line),
                "{}: {line}",
                t.name
            );
        }
    }

    #[test]
    fn forbidden_calls_never_appear_in_templates() {
        for t in ALL {
            assert_read_only(&t);
        }
        // load_asset: uniquement dans le gabarit d'inspection ciblée, une seule fois, jamais dans une boucle
        for t in ALL {
            let n = t.source().matches("load_asset(").count();
            assert_eq!(
                n,
                usize::from(t.name == "blueprint_variables"),
                "{}",
                t.name
            );
        }
        let bp = BLUEPRINT_VARIABLES.source();
        let before = &bp[..bp.find("load_asset(").unwrap()];
        let last_line = before.lines().last().unwrap_or("");
        assert!(
            !last_line.contains("for "),
            "load_asset ne doit pas être dans une boucle"
        );
    }

    fn python() -> Option<String> {
        ["python3", "python"].iter().find_map(|p| {
            Command::new(p)
                .arg("--version")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|_| (*p).to_owned())
        })
    }

    #[test]
    fn templates_are_valid_python() {
        let Some(py) = python() else {
            eprintln!("SKIPPED templates_are_valid_python: python3 absent");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        for t in ALL {
            let file = dir.path().join(format!("{}.py", t.name));
            std::fs::write(
                &file,
                t.render(&json!({"object_path": "/Game/X.X", "limit": 1})),
            )
            .unwrap();
            let out = Command::new(&py)
                .args(["-I", "-c", "import sys; compile(open(sys.argv[1], encoding='utf-8').read(), sys.argv[1], 'exec')"])
                .arg(&file)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}: {}",
                t.name,
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// Faux module `unreal` minimal: exécute vraiment les gabarits (hors éditeur) pour vérifier le
    /// décodage des paramètres et le format de sortie.
    const FAKE_UNREAL: &str = r##"
import sys, types
unreal = types.ModuleType("unreal")
class _Path:
    def __init__(self, n, p): self.asset_name = n; self.package_name = p
class _Asset:
    def __init__(self, pkg, name, cls):
        self.package_name = pkg; self.asset_name = name; self.asset_class_path = _Path(cls, "/Script/Engine")
        self.package_path = pkg.rsplit("/", 1)[0]
    def is_valid(self): return True
    def get_tag_value(self, n): return "Valeur" if n == "Triangles" else None
ASSETS = [_Asset("/Game/Other/DT_Two", "DT_Two", "DataTable"), _Asset("/Game/Mods/test/SM_Box", "SM_Box", "StaticMesh"), _Asset("/Game/Mods/test/DT_One", "DT_One", "DataTable")]
class _Registry:
    def get_all_assets(self): return list(ASSETS)
    def get_assets(self, f): return [a for a in ASSETS if a.package_name.startswith(f.package_paths[0])]
    def get_asset_by_object_path(self, p): return next((a for a in ASSETS if a.package_name + "." + a.asset_name == p), None)
    def get_dependencies(self, pkg, opts): return ["/Script/Engine", "/Game/Other/DT_Two"]
    def is_loading_assets(self): return False
class AssetRegistryHelpers:
    @staticmethod
    def get_asset_registry(): return _Registry()
class ARFilter:
    def __init__(self, **kw): self.__dict__.update(kw)
class AssetRegistryDependencyOptions:
    def __init__(self, **kw): self.__dict__.update(kw)
class SystemLibrary:
    get_engine_version = staticmethod(lambda: "5.8.2-test")
    get_game_name = staticmethod(lambda: "ConanSandbox")
    get_project_directory = staticmethod(lambda: "D:/Fake/")
class _Cdo:
    def __init__(self, props): self.__dict__.update(props)
    def get_editor_property(self, n):
        if n not in self.__dict__: raise Exception("pas de propriété " + n)
        return self.__dict__[n]
class _Pin:
    def __init__(self, text): self.text = text
    def export_text(self): return self.text
class _Bp:
    def generated_class(self): return "GeneratedClass"
    def list_member_variable_names(self): return ["my_var", "/Game/P.P_C.Inherited", "Item Data Table"]
    def get_member_variable_type(self, n):
        if n == "Item Data Table": return _Pin('(PinCategory="object",PinSubCategory="",PinSubCategoryObject="/Script/CoreUObject.Class\'/Script/Engine.DataTable\'",ContainerType=None,bIsReference=False)')
        return _Pin('(PinCategory="int",PinSubCategory="",PinSubCategoryObject=None,ContainerType=Array,bIsReference=False)')
    def get_blueprint_variable_category(self, n): return "Cat"
    def get_blueprint_variable_replication(self, n): return "NONE"
def load_asset(p): return _Bp() if "BP_" in p else None
def get_default_object(c): return _Cdo({"my_var": [3], "Item Data Table": "table"})
for k, v in list(globals().items()):
    if not k.startswith("__") and k not in ("sys", "types", "unreal"): setattr(unreal, k, v)
sys.modules["unreal"] = unreal
"##;

    fn run_with_fake_unreal(template: &Template, params: &Value) -> Option<Value> {
        let py = python()?;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("script.py");
        std::fs::write(&file, format!("{FAKE_UNREAL}\n{}", template.render(params))).unwrap();
        let out = Command::new(&py).args(["-I"]).arg(&file).output().unwrap();
        assert!(
            out.status.success(),
            "{}: {}",
            template.name,
            String::from_utf8_lossy(&out.stderr)
        );
        Some(extract_payload(&String::from_utf8_lossy(&out.stdout)).unwrap())
    }

    #[test]
    fn templates_run_against_a_fake_unreal_and_return_json_between_markers() {
        if python().is_none() {
            eprintln!("SKIPPED templates_run_against_a_fake_unreal: python3 absent");
            return;
        }
        let status = run_with_fake_unreal(&STATUS, &json!({})).unwrap();
        assert_eq!(
            (
                status["engine_version"].as_str(),
                status["asset_count"].as_u64()
            ),
            (Some("5.8.2-test"), Some(3))
        );

        let found = run_with_fake_unreal(
            &SEARCH_ASSETS,
            &json!({"path_prefix": "Mods/test", "limit": 1}),
        )
        .unwrap();
        assert_eq!(
            (found["total"].as_u64(), found["returned"].as_u64()),
            (Some(2), Some(1))
        );
        assert_eq!(
            found["assets"][0]["object_path"],
            "/Game/Mods/test/DT_One.DT_One"
        );
        // tri par object_path (les assets du faux registre sont dans le désordre), avant la limite
        let all = run_with_fake_unreal(&SEARCH_ASSETS, &json!({"limit": 50})).unwrap();
        let paths: Vec<_> = all["assets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["object_path"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            paths,
            [
                "/Game/Mods/test/DT_One.DT_One",
                "/Game/Mods/test/SM_Box.SM_Box",
                "/Game/Other/DT_Two.DT_Two"
            ]
        );
        let first = run_with_fake_unreal(&SEARCH_ASSETS, &json!({"limit": 1})).unwrap();
        assert_eq!(
            (first["total"].as_u64(), first["assets"][0]["name"].as_str()),
            (Some(3), Some("DT_One"))
        );
        let found = run_with_fake_unreal(
            &SEARCH_ASSETS,
            &json!({"class_name": "datatable", "limit": 50}),
        )
        .unwrap();
        assert_eq!(found["total"], 2);
        let found = run_with_fake_unreal(&SEARCH_ASSETS, &json!({"name_contains": "BOX"})).unwrap();
        assert_eq!(found["assets"][0]["name"], "SM_Box");

        let d = run_with_fake_unreal(
            &DESCRIBE_ASSET,
            &json!({"object_path": "/Game/Mods/test/SM_Box.SM_Box"}),
        )
        .unwrap();
        assert_eq!(
            (d["class"].as_str(), d["dependency_count"].as_u64()),
            (Some("StaticMesh"), Some(2))
        );
        assert_eq!(d["tags"]["Triangles"], "Valeur");
        assert_eq!(d["tags"].as_object().unwrap().len(), 1);
        let missing =
            run_with_fake_unreal(&DESCRIBE_ASSET, &json!({"object_path": "/Game/Nope.Nope"}))
                .unwrap();
        assert!(missing["error"].as_str().unwrap().contains("introuvable"));

        let bp = run_with_fake_unreal(
            &BLUEPRINT_VARIABLES,
            &json!({"object_path": "/Game/BP_X.BP_X"}),
        )
        .unwrap();
        let names: Vec<_> = bp["variables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["my_var", "Item Data Table"]); // la variable héritée (nom qualifié) est exclue
        assert_eq!(
            (
                bp["own_variable_count"].as_u64(),
                bp["inherited_variable_count"].as_u64()
            ),
            (Some(2), Some(1))
        );
        assert_eq!(bp["variables"][0]["type"]["label"], "Array of int");
        assert_eq!(bp["variables"][0]["default"], json!([3]));
        assert_eq!(bp["variables"][1]["type"]["label"], "object<DataTable>");
        assert_eq!(bp["variables"][1]["category"], "Cat");
        let all = run_with_fake_unreal(
            &BLUEPRINT_VARIABLES,
            &json!({"object_path": "/Game/BP_X.BP_X", "include_inherited": true}),
        )
        .unwrap();
        assert_eq!(all["variables"].as_array().unwrap().len(), 3);
        assert_eq!(all["variables"][2]["inherited"], true);
        // pas un Blueprint chargeable
        let not_bp = run_with_fake_unreal(
            &BLUEPRINT_VARIABLES,
            &json!({"object_path": "/Game/SM_X.SM_X"}),
        )
        .unwrap();
        assert!(not_bp["error"].as_str().unwrap().contains("introuvable"));
    }

    #[test]
    fn hostile_parameters_come_back_unchanged_and_execute_nothing() {
        if python().is_none() {
            eprintln!("SKIPPED hostile_parameters: python3 absent");
            return;
        }
        let nasty = "a\"b'c\\d\n''' + __import__('os').system('touch /tmp/pwned') + '''é⚔\u{2028}__CONAN_PARAMS__";
        let r = run_with_fake_unreal(&SEARCH_ASSETS, &json!({"name_contains": nasty, "limit": 3}))
            .unwrap();
        assert_eq!(r["query"]["name_contains"], nasty);
        assert!(!std::path::Path::new("/tmp/pwned").exists());
    }
}
