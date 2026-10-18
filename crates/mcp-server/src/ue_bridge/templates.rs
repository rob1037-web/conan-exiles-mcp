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
import itertools
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


# Conversion bornée (règles de l'étape 2): jamais de copie complète, jamais de lecture par clé; un conteneur ou un
# struct n'est PAS déplié par défaut; déplié sur demande, au plus _MAX_ITEMS éléments, avec islice.
_MAX_ITEMS = 20
_MAX_TEXT = 2000
_MAX_BATCH = 60
_MAX_BUDGET_S = 2.0
_SEQUENCES = ("Array", "Set", "list", "tuple", "set", "frozenset")
_MAPS = ("Map", "dict")


def _budget_s():
    try:
        wanted = float(_PARAMS.get("budget_s", _MAX_BUDGET_S))
    except Exception:
        wanted = _MAX_BUDGET_S
    return max(0.05, min(wanted, _MAX_BUDGET_S))


def _window(total):
    start = max(0, int(_PARAMS.get("start", 0)))
    count = max(1, min(_MAX_BATCH, int(_PARAMS.get("batch", _MAX_BATCH))))
    return start, min(total, start + count)


def _len_of(value):
    try:
        return len(value)
    except Exception:
        return None


def _text(chars):
    if len(chars) <= _MAX_TEXT:
        return chars
    return {"truncated_text": True, "length": len(chars), "head": chars[:200]}


def _more(length, shown):
    return shown >= _MAX_ITEMS and (length is None or length > shown)


def _safe(value, expand=False, depth=0):
    if value is None or isinstance(value, (bool, int)):
        return value
    if isinstance(value, float):
        return value if value == value and value not in (float("inf"), float("-inf")) else str(value)
    if isinstance(value, str):
        return _text(value)
    kind = type(value).__name__
    if kind in _SEQUENCES:
        length = _len_of(value)
        if not expand or depth > 0:
            return {"not_expanded": True, "kind": kind, "length": length}
        items = [_safe(item, False, depth + 1) for item in itertools.islice(iter(value), _MAX_ITEMS)]
        return {"kind": kind, "length": length, "items": items, "more": _more(length, len(items))}
    if kind in _MAPS:
        length = _len_of(value)
        if not expand or depth > 0:
            return {"not_expanded": True, "kind": kind, "length": length}
        if hasattr(value, "items"):
            pairs = [[_safe(k, False, depth + 1), _safe(v, False, depth + 1)] for k, v in itertools.islice(iter(value.items()), _MAX_ITEMS)]
            return {"kind": kind, "length": length, "pairs": pairs, "more": _more(length, len(pairs))}
        keys = [_safe(k, False, depth + 1) for k in itertools.islice(iter(value), _MAX_ITEMS)]
        return {"kind": kind, "length": length, "keys": keys, "values_unavailable": True, "more": _more(length, len(keys))}
    struct_base = getattr(unreal, "StructBase", None)
    if struct_base is not None and isinstance(value, struct_base):
        if not expand or depth > 0:
            return {"not_expanded": True, "kind": "struct", "type": kind}
        fields = {}
        for field in dir(value):
            if field.startswith("_"):
                continue
            if len(fields) >= _MAX_ITEMS:
                return {"kind": "struct", "type": kind, "fields": fields, "more": True}
            try:
                inner = getattr(value, field)
            except Exception:
                continue
            if callable(inner):
                continue
            fields[field] = _safe(inner, False, depth + 1)
        return {"kind": "struct", "type": kind, "fields": fields, "more": False}
    if hasattr(value, "name") and hasattr(value, "value") and not callable(value.name):
        return kind + "." + str(value.name)
    if hasattr(value, "get_path_name"):
        try:
            return value.get_path_name()
        except Exception:
            return None
    try:
        return _text(str(value))
    except Exception:
        return {"unprintable": True, "kind": kind}


def _dirty_packages():
    utils = unreal.EditorLoadingAndSavingUtils
    names = []
    for getter in (utils.get_dirty_content_packages, utils.get_dirty_map_packages):
        names.extend(str(package.get_name()) for package in getter())
    return sorted(set(names))


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


def _asset_data(registry, object_path, warnings):
    # API non obsolète d'abord (SoftObjectPath); l'ancienne n'est qu'un repli signalé.
    try:
        return registry.k2_get_asset_by_object_path(unreal.SoftObjectPath(object_path))
    except Exception as exc:
        warnings.append("k2_get_asset_by_object_path: " + type(exc).__name__ + ": " + str(exc)[:120])
    return registry.get_asset_by_object_path(object_path)

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
    asset = _asset_data(registry, object_path, warnings)
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
            found, *rest = value
            if not found:
                continue
            value = rest[0] if rest else None
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
def _main():
    started = time.monotonic()
    budget = _budget_s()
    warnings = []
    object_path = _PARAMS["object_path"]
    include_inherited = bool(_PARAMS.get("include_inherited", False))
    expand = set(str(n) for n in _PARAMS.get("expand", []))
    # Un seul chargement d'asset par commande.
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
    selected = own_names + (inherited_names if include_inherited else [])
    start, stop = _window(len(selected))
    variables = []
    index = start
    budget_exceeded = False
    while index < stop:
        if index > start and time.monotonic() - started > budget:
            budget_exceeded = True
            break
        name = selected[index]
        index += 1
        plain = name.rsplit(".", 1)[-1]
        entry = {"name": name, "inherited": name.startswith("/")}
        pin = _try(warnings, "type:" + name, lambda: blueprint.get_member_variable_type(name))
        entry["type"] = _pin_type(pin) if pin is not None else None
        entry["category"] = _try(warnings, "category:" + name, lambda: _safe(blueprint.get_blueprint_variable_category(name)))
        entry["replication"] = _try(warnings, "replication:" + name, lambda: _safe(blueprint.get_blueprint_variable_replication(name)))
        entry["default_available"] = False
        entry["default"] = None
        entry["default_python_type"] = None
        if default_object is not None and not name.startswith("/"):
            try:
                value = default_object.get_editor_property(plain)
                entry["default"] = _safe(value, plain in expand)
                entry["default_python_type"] = type(value).__name__
                entry["default_available"] = True
            except Exception as exc:
                entry["default_error"] = type(exc).__name__
        variables.append(entry)
    dirty = _try(warnings, "dirty check", _dirty_packages)
    package = object_path.split(".")[0]
    result["package_marked_modified"] = (package in dirty) if dirty is not None else None
    result["dirty_packages"] = dirty
    result["own_variable_count"] = len(own_names)
    result["inherited_variable_count"] = len(inherited_names)
    result["total"] = len(selected)
    result["start"] = start
    result["next_index"] = index if index < len(selected) else None
    result["budget_exceeded"] = budget_exceeded
    result["elapsed_s"] = round(time.monotonic() - started, 3)
    result["variables"] = variables
    if _PARAMS.get("diagnostics"):
        result["diagnostics"] = {
            "generated_class": str(generated),
            "blueprint_members": [n for n in dir(blueprint) if not n.startswith("_")],
        }
    _emit(result)


_main()
"##;

/// Valeurs par défaut d'un Blueprint (membres ou composants), LECTURE SEULE, par fenêtres.
/// Règles de l'étape 2: un seul chargement d'asset par commande, au plus 60 éléments par commande,
/// budget de 2 s vérifié entre deux éléments (résultat partiel + `next_index`), conteneurs et structs
/// non dépliés par défaut (voir `_safe`).
const DEFAULTS_BODY: &str = r##"
def _split_name(qualified):
    plain = qualified.rsplit(".", 1)[-1]
    owner = qualified[: -(len(plain) + 1)] if qualified.startswith("/") else ""
    return plain, owner


def _components(blueprint, warnings):
    subsystem = unreal.get_engine_subsystem(unreal.SubobjectDataSubsystem)
    library = unreal.SubobjectDataBlueprintFunctionLibrary
    found = []
    for handle in subsystem.k2_gather_subobject_data_for_blueprint(blueprint):
        data = library.get_data(handle)
        found.append((str(library.get_variable_name(data)), data))
    return library, found


def _member_item(blueprint, cdo, qualified, plain, owner, expand):
    item = {"name": plain, "owner": owner}
    try:
        item["type"] = _pin_type(blueprint.get_member_variable_type(qualified))["label"]
    except Exception as exc:
        item["type_error"] = type(exc).__name__
    try:
        value = cdo.get_editor_property(plain)
    except Exception as exc:
        item["unreadable"] = type(exc).__name__ + ": " + str(exc)[:120]
        return item
    item["python_type"] = type(value).__name__
    item["value"] = _safe(value, plain in expand)
    return item


def _main():
    started = time.monotonic()
    budget = _budget_s()
    warnings = []
    object_path = _PARAMS["object_path"]
    section = _PARAMS.get("section", "members")
    expand = set(str(n) for n in _PARAMS.get("expand", []))
    # Un seul chargement d'asset par commande.
    blueprint = unreal.load_asset(object_path)
    if blueprint is None:
        _emit({"error": "Asset introuvable ou non chargeable: " + object_path})
        return
    if not hasattr(blueprint, "list_member_variable_names"):
        _emit({"error": "Cet asset n'est pas un Blueprint (type Python " + type(blueprint).__name__ + ")"})
        return
    generated = _try(warnings, "generated_class", lambda: blueprint.generated_class())
    cdo = _try(warnings, "get_default_object", lambda: unreal.get_default_object(generated)) if generated is not None else None
    result = {"object_path": object_path, "section": section, "asset_python_type": type(blueprint).__name__, "warnings": warnings}
    missing = []
    # entries: éléments à lire, dans un ordre stable; chaque élément est lu par produce(i).
    if section == "members":
        if cdo is None:
            _emit({"error": "Objet par défaut (CDO) introuvable pour " + object_path})
            return
        entries = []
        for qualified in blueprint.list_member_variable_names():
            qualified = str(qualified)
            plain, owner = _split_name(qualified)
            entries.append((qualified, plain, owner))
        wanted = _PARAMS.get("names")
        if wanted is not None:
            wanted = [str(n) for n in wanted]
            present = set(e[1] for e in entries)
            missing = [n for n in wanted if n not in present]
            keep = set(wanted)
            entries = [e for e in entries if e[1] in keep]

        def produce(i):
            qualified, plain, owner = entries[i]
            return _member_item(blueprint, cdo, qualified, plain, owner, expand)
    elif section == "components":
        library, found = _components(blueprint, warnings)
        entries = found

        def produce(i):
            variable, data = entries[i]
            item = {"name": variable, "owner": ""}
            item["is_component"] = bool(library.is_component(data))
            item["is_inherited"] = bool(library.is_inherited_component(data))
            item["is_native"] = bool(library.is_native_component(data))
            obj = library.get_object_for_blueprint(data, blueprint)
            item["class"] = type(obj).__name__ if obj is not None else None
            item["object"] = obj.get_path_name() if obj is not None and hasattr(obj, "get_path_name") else None
            return item
    elif section == "component_values":
        library, found = _components(blueprint, warnings)
        wanted_component = str(_PARAMS.get("component", ""))
        target = None
        for variable, data in found:
            if variable == wanted_component:
                target = library.get_object_for_blueprint(data, blueprint)
                break
        if target is None:
            _emit({"error": "Composant introuvable: " + repr(wanted_component) + " (composants: " + ", ".join(v for v, _ in found) + ")"})
            return
        wanted = _PARAMS.get("names")
        if wanted is not None:
            names = [str(n) for n in wanted]
        else:
            names = [n for n in dir(target) if not n.startswith("_")]
        entries = names

        def produce(i):
            name = entries[i]
            item = {"name": name, "owner": ""}
            try:
                inner = getattr(target, name) if wanted is None else None
                if wanted is None and callable(inner):
                    return None
                value = target.get_editor_property(name)
            except Exception as exc:
                item["unreadable"] = type(exc).__name__ + ": " + str(exc)[:120]
                return item
            item["python_type"] = type(value).__name__
            item["value"] = _safe(value, name in expand)
            return item
    else:
        _emit({"error": "section inconnue: " + repr(section) + " (members, components, component_values)"})
        return
    start, stop = _window(len(entries))
    items = []
    index = start
    budget_exceeded = False
    while index < stop:
        if index > start and time.monotonic() - started > budget:
            budget_exceeded = True
            break
        item = produce(index)
        index += 1
        if item is not None:
            items.append(item)
    result["total"] = len(entries)
    result["start"] = start
    result["next_index"] = index if index < len(entries) else None
    result["budget_exceeded"] = budget_exceeded
    result["missing_names"] = missing
    result["items"] = items
    result["dirty_packages"] = _try(warnings, "dirty check", _dirty_packages)
    result["elapsed_s"] = round(time.monotonic() - started, 3)
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

pub const DEFAULTS: Template = Template {
    name: "defaults",
    body: DEFAULTS_BODY,
};

pub const ALL: [Template; 5] = [
    STATUS,
    SEARCH_ASSETS,
    DESCRIBE_ASSET,
    BLUEPRINT_VARIABLES,
    DEFAULTS,
];

impl Template {
    #[cfg(any(test, feature = "test-support"))]
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
        assert_read_only_except(t, &[]);
    }

    /// Même contrôle, en admettant explicitement certains motifs (la trace durable des sondes).
    pub(crate) fn assert_read_only_except(t: &Template, allowed: &[&str]) {
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
        for bad in FORBIDDEN.iter().filter(|b| !allowed.contains(b)) {
            assert!(!src.contains(bad), "gabarit {}: contient {bad:?}", t.name);
        }
        // seuls `import json`, `import re`, `import sys`, `import unreal` sont admis (en tête de fichier)
        for line in src.lines().filter(|l| {
            l.trim_start().starts_with("import ") || l.trim_start().starts_with("from ")
        }) {
            assert!(
                [
                    "import itertools",
                    "import json",
                    "import re",
                    "import sys",
                    "import time",
                    "import unreal"
                ]
                .contains(&line)
                    || (line == "import os" && allowed.contains(&"os.")),
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
        // load_asset: uniquement dans les gabarits d'inspection ciblée, une seule fois par commande,
        // jamais dans une boucle
        for t in ALL {
            let n = t.source().matches("load_asset(").count();
            assert_eq!(
                n,
                usize::from(matches!(t.name, "blueprint_variables" | "defaults")),
                "{}",
                t.name
            );
        }
        for t in [BLUEPRINT_VARIABLES, DEFAULTS] {
            let src = t.source();
            let line = src.lines().find(|l| l.contains("load_asset(")).unwrap();
            assert!(
                line.starts_with("    blueprint = ") && !line.starts_with("     "),
                "{}: load_asset doit être au niveau de la fonction, hors boucle: {line}",
                t.name
            );
        }
    }

    /// Règles de l'étape 2, vérifiées sur le texte: aucune itération complète d'un conteneur
    /// (`list(...)`, `tuple(...)`, `dict(...)`), aucune lecture `valeur[clé]` sur une map, aucune API
    /// obsolète hors du repli signalé, aucun `get_object` obsolète.
    #[test]
    fn templates_never_materialize_or_index_a_container() {
        for t in ALL {
            let src = t.source();
            for bad in [
                "list(",
                "tuple(",
                "dict(",
                "value[",
                "sorted(value",
                "set(value",
                "[key]",
            ] {
                assert!(!src.contains(bad), "{}: contient {bad:?}", t.name);
            }
            assert!(
                !src.contains(".get_object("),
                "{}: get_object obsolète",
                t.name
            );
            assert!(
                !src.contains("EditorAssetLibrary"),
                "{}: EditorAssetLibrary interdit",
                t.name
            );
        }
        // l'ancienne API n'existe que dans le repli de _asset_data, une seule fois pour tout le prélude
        let prelude = STATUS.source();
        assert_eq!(
            prelude
                .matches("registry.get_asset_by_object_path(")
                .count(),
            1
        );
        assert!(prelude.contains("k2_get_asset_by_object_path(unreal.SoftObjectPath("));
        for t in ALL {
            assert_eq!(
                t.source()
                    .matches("registry.get_asset_by_object_path(")
                    .count(),
                1,
                "{}",
                t.name
            );
        }
        // les lectures de conteneurs passent par islice, au plus _MAX_ITEMS
        let src = DEFAULTS.source();
        assert!(src.contains("itertools.islice(iter(value), _MAX_ITEMS)"));
        assert!(src.contains("_MAX_BATCH = 60"));
        assert!(src.contains("_MAX_BUDGET_S = 2.0"));
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
    def k2_get_asset_by_object_path(self, sp): return next((a for a in ASSETS if a.package_name + "." + a.asset_name == sp.path), None)
    def get_dependencies(self, pkg, opts): return ["/Script/Engine", "/Game/Other/DT_Two"]
    def is_loading_assets(self): return False
class AssetRegistryHelpers:
    @staticmethod
    def get_asset_registry(): return _Registry()
class SoftObjectPath:
    def __init__(self, p): self.path = p
class StructBase: pass
class Vector(StructBase):
    def __init__(self): self.x = 1.0; self.y = 2.0; self.z = 3.0
    def length(self): return 3.7
class EditorLoadingAndSavingUtils:
    get_dirty_content_packages = staticmethod(lambda: [])
    get_dirty_map_packages = staticmethod(lambda: [])
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
import time as _time
ITER_OK = __ITER_OK__
class Array:
    """Faux tableau de 100 000 éléments: toute itération complète ou lecture par index est une faute."""
    def __len__(self): return 100000
    def __iter__(self):
        if not ITER_OK: raise AssertionError("itération interdite")
        for i in range(100000):
            if i >= 21: raise AssertionError("itération au-delà de 20 éléments")
            yield i
    def __getitem__(self, i): raise AssertionError("lecture par index interdite")
class Map:
    def __len__(self): return 100000
    def __iter__(self):
        if not ITER_OK: raise AssertionError("itération interdite")
        for i in range(100000):
            if i >= 21: raise AssertionError("itération au-delà de 20 clés")
            yield "k" + str(i)
    def items(self):
        if not ITER_OK: raise AssertionError("items() interdit")
        return (("k" + str(i), i) for i in range(100000))
    def __getitem__(self, k): raise AssertionError("valeur[clé] interdit sur une map")
class _Cls:
    def __init__(self, values, delay=0.0): self.values = values; self.delay = delay
class _Prop:
    def __init__(self, values, delay): self.values = values; self.delay = delay
class _Big:
    """Blueprint à nombreux membres, lectures éventuellement lentes."""
    def __init__(self, count, delay=0.0, special=False):
        self.names = ["P" + str(i) for i in range(count)]
        self.delay = delay
        self.special = special
        if special: self.names += ["BigArray", "BigMap", "Loc", "Locked"]
    def generated_class(self): return _Cls({n: i for i, n in enumerate(self.names)}, self.delay)
    def list_member_variable_names(self): return list(self.names)
    def get_member_variable_type(self, n): return _Pin('(PinCategory="int",PinSubCategory="",PinSubCategoryObject=None,ContainerType=None)')
class _BigCdo:
    def __init__(self, cls): self.cls = cls
    def get_editor_property(self, n):
        if self.cls.delay: _time.sleep(self.cls.delay)
        if n == "BigArray": return Array()
        if n == "BigMap": return Map()
        if n == "Loc": return Vector()
        if n == "Locked": raise Exception("propriété illisible")
        return self.cls.values[n]
class _Comp:
    def __init__(self): self.props = {"relative_scale": 2.0, "mobility": "Static"}
    def get_editor_property(self, n):
        if n not in self.props: raise Exception("pas de propriété " + n)
        return self.props[n]
    def get_path_name(self): return "/Game/BP_X.BP_X_C:StaticMesh_GEN_VARIABLE"
    relative_scale = 2.0
    mobility = "Static"
    def set_mobility(self, v): pass
class _Handle:
    def __init__(self, name, inherited, obj): self.name = name; self.inherited = inherited; self.obj = obj
class _Subsystem:
    def k2_gather_subobject_data_for_blueprint(self, bp): return [_Handle("DefaultSceneRoot", False, _Comp()), _Handle("StaticMesh", True, _Comp())]
class SubobjectDataSubsystem: pass
def get_engine_subsystem(cls): return _Subsystem()
class SubobjectDataBlueprintFunctionLibrary:
    get_data = staticmethod(lambda h: h)
    get_variable_name = staticmethod(lambda d: d.name)
    is_component = staticmethod(lambda d: True)
    is_inherited_component = staticmethod(lambda d: d.inherited)
    is_native_component = staticmethod(lambda d: False)
    get_object_for_blueprint = staticmethod(lambda d, bp: d.obj)
def load_asset(p):
    if "BP_MANY" in p: return _Big(130)
    if "BP_SLOW" in p: return _Big(10, 0.04)
    if "BP_BIG" in p: return _Big(2, 0.0, True)
    return _Bp() if "BP_" in p else None
def get_default_object(c):
    if isinstance(c, _Cls): return _BigCdo(c)
    return _Cdo({"my_var": [3], "Item Data Table": "table"})
for k, v in list(globals().items()):
    if not k.startswith("__") and k not in ("sys", "types", "unreal"): setattr(unreal, k, v)
sys.modules["unreal"] = unreal
"##;

    fn run_with_fake_unreal(template: &Template, params: &Value) -> Option<Value> {
        run_with_fake_unreal_iter(template, params, true)
    }

    /// `iter_ok = false`: le faux conteneur de 100 000 éléments refuse toute itération.
    fn run_with_fake_unreal_iter(
        template: &Template,
        params: &Value,
        iter_ok: bool,
    ) -> Option<Value> {
        let py = python()?;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("script.py");
        let fake = FAKE_UNREAL.replace("__ITER_OK__", if iter_ok { "True" } else { "False" });
        std::fs::write(&file, format!("{fake}\n{}", template.render(params))).unwrap();
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
        // un conteneur n'est pas déplié par défaut; sur demande (expand), il l'est, borné
        assert_eq!(
            bp["variables"][0]["default"],
            json!({"not_expanded": true, "kind": "list", "length": 1})
        );
        let expanded = run_with_fake_unreal(
            &BLUEPRINT_VARIABLES,
            &json!({"object_path": "/Game/BP_X.BP_X", "expand": ["my_var"]}),
        )
        .unwrap();
        assert_eq!(
            expanded["variables"][0]["default"],
            json!({"kind": "list", "length": 1, "items": [3], "more": false})
        );
        assert_eq!(bp["dirty_packages"], json!([]));
        assert_eq!(bp["next_index"], Value::Null);
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

    fn items_of(r: &Value) -> Vec<&Value> {
        r["items"].as_array().unwrap().iter().collect()
    }

    /// Étape 2: un faux conteneur de 100 000 éléments ne doit JAMAIS être parcouru en entier ni indexé.
    #[test]
    fn a_container_of_100000_elements_is_never_iterated_by_default_and_capped_when_expanded() {
        if python().is_none() {
            eprintln!("SKIPPED container_of_100000: python3 absent");
            return;
        }
        // défaut: zéro itération (le faux conteneur lève s'il est itéré), longueur seulement
        let r = run_with_fake_unreal_iter(
            &DEFAULTS,
            &json!({"object_path": "/Game/BP_BIG.BP_BIG"}),
            false,
        )
        .unwrap();
        let by_name = |n: &str| {
            items_of(&r)
                .into_iter()
                .find(|i| i["name"] == n)
                .unwrap_or_else(|| panic!("{n}: {r}"))
                .clone()
        };
        assert_eq!(
            by_name("BigArray")["value"],
            json!({"not_expanded": true, "kind": "Array", "length": 100000})
        );
        assert_eq!(
            by_name("BigMap")["value"],
            json!({"not_expanded": true, "kind": "Map", "length": 100000})
        );
        assert_eq!(
            by_name("Loc")["value"],
            json!({"not_expanded": true, "kind": "struct", "type": "Vector"})
        );
        assert!(by_name("Locked")["unreadable"]
            .as_str()
            .unwrap()
            .contains("illisible"));
        assert_eq!(by_name("P0")["value"], 0);

        // dépliage demandé: au plus 20 éléments, "more" signalé, jamais d'index
        let r = run_with_fake_unreal_iter(
            &DEFAULTS,
            &json!({"object_path": "/Game/BP_BIG.BP_BIG", "expand": ["BigArray", "BigMap", "Loc"]}),
            true,
        )
        .unwrap();
        let get = |n: &str| {
            items_of(&r)
                .into_iter()
                .find(|i| i["name"] == n)
                .unwrap()
                .clone()
        };
        let array = &get("BigArray")["value"];
        assert_eq!(array["items"].as_array().unwrap().len(), 20);
        assert_eq!(
            (array["length"].as_u64(), array["more"].as_bool()),
            (Some(100000), Some(true))
        );
        let map = &get("BigMap")["value"];
        assert_eq!(map["pairs"].as_array().unwrap().len(), 20);
        assert_eq!(map["pairs"][3], json!(["k3", 3]));
        assert_eq!(map["more"], true);
        let loc = &get("Loc")["value"];
        assert_eq!(loc["fields"], json!({"x": 1.0, "y": 2.0, "z": 3.0}));
        assert_eq!(loc["more"], false);
        // le résultat reste petit
        assert!(serde_json::to_string(&r).unwrap().len() < 4000);
    }

    #[test]
    fn defaults_come_by_batches_of_60_and_stop_at_the_budget_with_next_index() {
        if python().is_none() {
            eprintln!("SKIPPED defaults_batches: python3 absent");
            return;
        }
        // 130 membres: lots de 60 + 60 + 10, ordre stable, aucune perte
        let mut seen = Vec::new();
        let mut start = 0u64;
        let mut calls = 0;
        loop {
            let r = run_with_fake_unreal(
                &DEFAULTS,
                &json!({"object_path": "/Game/BP_MANY.BP_MANY", "start": start, "batch": 500}),
            )
            .unwrap();
            calls += 1;
            assert!(items_of(&r).len() <= 60, "lot > 60: {}", items_of(&r).len());
            assert_eq!(r["total"], 130);
            seen.extend(
                items_of(&r)
                    .iter()
                    .map(|i| i["name"].as_str().unwrap().to_owned()),
            );
            match r["next_index"].as_u64() {
                Some(n) => start = n,
                None => break,
            }
        }
        assert_eq!(calls, 3);
        let expected: Vec<String> = (0..130).map(|i| format!("P{i}")).collect();
        assert_eq!(seen, expected);

        // 10 membres à 0,04 s, budget 0,05 s: résultat partiel, reprise par next_index, rien d'oublié
        let mut seen = Vec::new();
        let mut start = 0u64;
        let mut partials = 0;
        loop {
            let r = run_with_fake_unreal(
                &DEFAULTS,
                &json!({"object_path": "/Game/BP_SLOW.BP_SLOW", "start": start, "budget_s": 0.05}),
            )
            .unwrap();
            assert!(!items_of(&r).is_empty(), "au moins un élément par commande");
            if r["budget_exceeded"] == true {
                partials += 1;
                assert!(items_of(&r).len() < 10);
            }
            seen.extend(
                items_of(&r)
                    .iter()
                    .map(|i| i["name"].as_str().unwrap().to_owned()),
            );
            match r["next_index"].as_u64() {
                Some(n) => start = n,
                None => break,
            }
        }
        assert!(partials >= 1, "le budget aurait dû être dépassé");
        let expected: Vec<String> = (0..10).map(|i| format!("P{i}")).collect();
        assert_eq!(seen, expected);
        // un budget demandé au-delà du plafond de 2 s est ramené à 2 s
        let r = run_with_fake_unreal(
            &DEFAULTS,
            &json!({"object_path": "/Game/BP_SLOW.BP_SLOW", "budget_s": 999}),
        )
        .unwrap();
        assert_eq!(r["next_index"], Value::Null);
        assert_eq!(items_of(&r).len(), 10);
    }

    #[test]
    fn defaults_select_names_report_missing_and_read_components() {
        if python().is_none() {
            eprintln!("SKIPPED defaults_names_components: python3 absent");
            return;
        }
        let r = run_with_fake_unreal(
            &DEFAULTS,
            &json!({"object_path": "/Game/BP_MANY.BP_MANY", "names": ["P7", "Nope", "P3"]}),
        )
        .unwrap();
        let names: Vec<_> = items_of(&r)
            .iter()
            .map(|i| i["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["P3", "P7"]);
        assert_eq!(r["missing_names"], json!(["Nope"]));
        assert_eq!(items_of(&r)[0]["type"], "int");
        assert_eq!(r["dirty_packages"], json!([]));

        let r = run_with_fake_unreal(
            &DEFAULTS,
            &json!({"object_path": "/Game/BP_X.BP_X", "section": "components"}),
        )
        .unwrap();
        assert_eq!(items_of(&r).len(), 2);
        assert_eq!(items_of(&r)[1]["name"], "StaticMesh");
        assert_eq!(items_of(&r)[1]["is_inherited"], true);
        assert_eq!(items_of(&r)[1]["class"], "_Comp");

        let r = run_with_fake_unreal(
            &DEFAULTS,
            &json!({"object_path": "/Game/BP_X.BP_X", "section": "component_values", "component": "StaticMesh"}),
        )
        .unwrap();
        let got: Vec<_> = items_of(&r)
            .iter()
            .map(|i| i["name"].as_str().unwrap())
            .collect();
        assert!(
            got.contains(&"relative_scale") && got.contains(&"mobility"),
            "{got:?}"
        );
        assert!(!got.contains(&"set_mobility"), "les méthodes sont ignorées");
        let unknown = run_with_fake_unreal(
            &DEFAULTS,
            &json!({"object_path": "/Game/BP_X.BP_X", "section": "component_values", "component": "Nope"}),
        )
        .unwrap();
        assert!(unknown["error"]
            .as_str()
            .unwrap()
            .contains("Composant introuvable"));
        let bad = run_with_fake_unreal(
            &DEFAULTS,
            &json!({"object_path": "/Game/BP_X.BP_X", "section": "x"}),
        )
        .unwrap();
        assert!(bad["error"].as_str().unwrap().contains("section inconnue"));
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
