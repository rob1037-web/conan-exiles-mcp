//! Sondes de la Phase 4b (étape 1): introspection **en lecture seule** du Devkit pour établir ce
//! que Python peut lire sur les valeurs par défaut des Blueprints et de leurs composants.
//!
//! Elles ne sont jamais exposées par un outil MCP et ne sont pas dans le binaire livré (feature
//! `test-support`, comme le faux éditeur); seul l'exemple `ue_spike` les lance. Mêmes interdits que les
//! gabarits des outils (voir `templates`): aucune écriture, aucun save/compile/set.

use super::templates::Template;

const PROBE_BODY: &str = r##"
import os

# --- Trace durable (sonde seulement) -------------------------------------------------------------
# Écrit une ligne JSON par étape, puis flush + fsync: la trace survit à un plantage de l'éditeur.
# Le chemin est limité à un fichier .log sous D:/ConanMod/ (jamais sous un dossier de mod).
_TRACE_FILE = None
_CTX = {}


def _trace_open(path):
    global _TRACE_FILE
    ok = isinstance(path, str) and path.startswith("D:/ConanMod/") and path.endswith(".log")
    if not ok or ".." in path or "/Mods/" in path or "/csv/" in path:
        raise ValueError("trace_path refusé: " + str(path))
    _TRACE_FILE = open(path, "a", encoding="utf-8")


def _trace(**fields):
    if _TRACE_FILE is None:
        return
    _TRACE_FILE.write(json.dumps(fields, ensure_ascii=False, default=str) + "\n")
    _TRACE_FILE.flush()
    os.fsync(_TRACE_FILE.fileno())


def _plain_t(value, depth=0):
    """Même logique que _plain, avec une trace AVANT chaque opération susceptible de bloquer."""
    if value is None or isinstance(value, (bool, int, str)):
        return value
    if isinstance(value, float):
        return value if value == value and value not in (float("inf"), float("-inf")) else str(value)
    kind = type(value).__name__
    _trace(step="enter", depth=depth, kind=kind, **_CTX)
    _trace(step="hasattr_name_value", depth=depth, kind=kind, **_CTX)
    if hasattr(value, "name") and hasattr(value, "value") and not callable(value.name):
        return type(value).__name__ + "." + str(value.name)
    _trace(step="hasattr_get_path_name", depth=depth, kind=kind, **_CTX)
    if hasattr(value, "get_path_name"):
        _trace(step="get_path_name", depth=depth, kind=kind, **_CTX)
        return value.get_path_name()
    if depth < 2 and (isinstance(value, (list, tuple)) or kind in ("Array", "Set")):
        _trace(step="list_begin", depth=depth, kind=kind, **_CTX)
        items = list(value)[:20]
        _trace(step="list_done", depth=depth, kind=kind, n=len(items), **_CTX)
        converted = []
        for index, item in enumerate(items):
            _trace(step="element", depth=depth + 1, container=kind, index=index, elem_type=type(item).__name__, **_CTX)
            converted.append(_plain_t(item, depth + 1))
        return converted
    if depth < 2 and kind == "Map":
        _trace(step="map_begin", depth=depth, kind=kind, **_CTX)
        keys = list(value)[:20]
        _trace(step="map_keys_done", depth=depth, n=len(keys), **_CTX)
        converted = {}
        for index, key in enumerate(keys):
            _trace(step="map_key", depth=depth + 1, index=index, key_type=type(key).__name__, **_CTX)
            key_text = str(key)
            _trace(step="map_lookup", depth=depth + 1, index=index, key=key_text[:80], **_CTX)
            sub = value[key]
            _trace(step="map_value", depth=depth + 1, index=index, value_type=type(sub).__name__, **_CTX)
            converted[key_text] = _plain_t(sub, depth + 1)
        return converted
    _trace(step="str", depth=depth, kind=kind, **_CTX)
    return str(value)


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
        return [_plain(item, depth + 1) for item in list(value)[:20]]
    if depth < 2 and kind == "Map":
        return {str(key): _plain(value[key], depth + 1) for key in list(value)[:20]}
    return str(value)


def _members(obj, needles=None):
    names = []
    for name in dir(obj):
        if name.startswith("_"):
            continue
        if needles and not any(n in name.lower() for n in needles):
            continue
        names.append(name)
    return names


def _quoted(tag_value):
    if tag_value and "'" in tag_value:
        parts = tag_value.split("'")
        if len(parts) >= 2:
            return parts[1]
    return tag_value


def _public_values(warnings, obj):
    names = []
    for name in dir(obj):
        if name.startswith("_"):
            continue
        try:
            if callable(getattr(obj, name)):
                continue
        except Exception:
            continue
        names.append(name)
    return names


def _dirty_packages():
    utils = unreal.EditorLoadingAndSavingUtils
    names = []
    for getter in (utils.get_dirty_content_packages, utils.get_dirty_map_packages):
        names.extend(str(package.get_name()) for package in getter())
    return sorted(set(names))


def _snake_guess(name):
    out = []
    for i, ch in enumerate(name):
        if ch.isupper() and i > 0 and (not name[i - 1].isupper() or (i + 1 < len(name) and name[i + 1].islower())):
            out.append("_")
        out.append(ch.lower())
    return "".join(out)


def _main():
    warnings = []
    tasks = _PARAMS.get("tasks", [])
    object_path = _PARAMS["object_path"]
    result = {"object_path": object_path, "warnings": warnings}
    registry = unreal.AssetRegistryHelpers.get_asset_registry()
    blueprint = unreal.load_asset(object_path)
    if blueprint is None:
        _emit({"error": "Asset introuvable ou non chargeable: " + object_path})
        return
    generated = _try(warnings, "generated_class", lambda: blueprint.generated_class())
    cdo = _try(warnings, "cdo", lambda: unreal.get_default_object(generated))
    result["generated_class"] = str(generated)
    result["cdo_python_type"] = type(cdo).__name__

    if "dirs" in tasks:
        targets = {
            "unreal": unreal,
            "blueprint": blueprint,
            "generated_class": generated,
            "cdo": cdo,
        }
        for lib in _PARAMS.get("libraries", []):
            targets[lib] = getattr(unreal, lib, None)
        needles = [n.lower() for n in _PARAMS.get("filter", [])] or None
        result["dirs"] = {name: (_members(obj, needles) if obj is not None else None) for name, obj in targets.items() if name in _PARAMS.get("dir_targets", list(targets))}

    if "cdo_props" in tasks:
        parent_tag = None
        asset_data = registry.get_asset_by_object_path(object_path)
        if asset_data is not None and asset_data.is_valid():
            parent_tag = _quoted(_try(warnings, "tag ParentClass", lambda: asset_data.get_tag_value("ParentClass")))
        parent_cdo = None
        if parent_tag:
            parent_class = _try(warnings, "parent class", lambda: unreal.load_object(None, parent_tag))
            if parent_class is not None:
                parent_cdo = _try(warnings, "parent cdo", lambda: unreal.get_default_object(parent_class))
        result["parent_class"] = parent_tag
        props = []
        for name in _public_values(warnings, cdo):
            entry = {"name": name}
            try:
                value = cdo.get_editor_property(name)
                entry["python_type"] = type(value).__name__
                entry["value"] = _plain(value)
            except Exception as exc:
                entry["error"] = type(exc).__name__ + ": " + str(exc)[:120]
                props.append(entry)
                continue
            if hasattr(cdo, "is_editor_property_overridden"):
                entry["overridden"] = _try(warnings, "overridden:" + name, lambda: bool(cdo.is_editor_property_overridden(name)))
            if parent_cdo is not None:
                try:
                    parent_value = parent_cdo.get_editor_property(name)
                    entry["differs_from_parent"] = _plain(parent_value) != entry["value"]
                    if entry["differs_from_parent"]:
                        entry["parent_value"] = _plain(parent_value)
                except Exception:
                    entry["differs_from_parent"] = None
            props.append(entry)
        result["cdo_property_count"] = len(props)
        wanted = [w.lower().replace(" ", "").replace("_", "") for w in _PARAMS.get("find", [])]
        if wanted:
            result["found"] = [p for p in props if any(w in p["name"].lower().replace(" ", "").replace("_", "") for w in wanted)]
        else:
            result["cdo_properties"] = props

    if "components" in tasks:
        comps = []
        try:
            for comp in cdo.get_components_by_class(unreal.ActorComponent):
                item = {"name": comp.get_name(), "class": type(comp).__name__, "path": comp.get_path_name()}
                comps.append(item)
        except Exception as exc:
            warnings.append("get_components_by_class: " + type(exc).__name__ + ": " + str(exc))
        result["cdo_components"] = comps

    if "member_names" in tasks:
        needles = [n.lower().replace(" ", "").replace("_", "") for n in _PARAMS.get("find", [])]
        found = []
        for name in _try(warnings, "list_member_variable_names", lambda: [str(n) for n in blueprint.list_member_variable_names()], []):
            flat = name.lower().replace(" ", "").replace("_", "")
            if any(n in flat for n in needles):
                entry = {"name": name}
                entry["category"] = _try(warnings, "cat", lambda: _plain(blueprint.get_blueprint_variable_category(name)))
                entry["type"] = _try(warnings, "type", lambda: _plain(blueprint.get_member_variable_type(name).export_text()))
                found.append(entry)
        result["member_names"] = found

    if "inventory" in tasks:
        asset_data = registry.get_asset_by_object_path(object_path)
        parent_tag = None
        if asset_data is not None and asset_data.is_valid():
            parent_tag = _quoted(_try(warnings, "tag ParentClass", lambda: asset_data.get_tag_value("ParentClass")))
        parent_cdo = None
        if parent_tag:
            parent_class = _try(warnings, "parent class", lambda: unreal.load_object(None, parent_tag))
            if parent_class is not None:
                parent_cdo = _try(warnings, "parent cdo", lambda: unreal.get_default_object(parent_class))
        result["parent_class"] = parent_tag
        names = _try(warnings, "names", lambda: [str(n) for n in blueprint.list_member_variable_names()], [])
        python_names = set(_public_values(warnings, cdo))
        readable = 0
        unreadable = []
        rows = []
        trace_members = bool(_PARAMS.get("trace_members"))
        tracing = bool(_PARAMS.get("trace_path"))
        if tracing:
            _trace_open(_PARAMS["trace_path"])
            _trace(step="start", object_path=object_path, members=len(names))
        convert = _plain_t if tracing else _plain
        for index, qualified in enumerate(names):
            if trace_members:
                print("CONAN_TRACE inventory.member " + str(index) + " " + qualified)
            plain = qualified.rsplit(".", 1)[-1]
            owner = qualified[: -(len(plain) + 1)] if "." in qualified else ""
            entry = {"name": plain, "owner": owner, "in_python_dir": plain in python_names or _snake_guess(plain) in python_names}
            _CTX.clear()
            _CTX.update({"i": index, "name": plain, "owner": owner})
            _trace(step="member_begin", i=index, name=plain, owner=owner)
            pin = _try(warnings, "type:" + qualified, lambda: blueprint.get_member_variable_type(qualified).export_text())
            _CTX["type"] = pin
            _trace(step="member_typed", i=index, name=plain, owner=owner, type=pin)
            if pin:
                category = re.search(r'PinCategory="([^"]*)"', pin)
                sub = re.search(r'PinSubCategory="([^"]*)"', pin)
                container = re.search(r'ContainerType=(\w+)', pin)
                entry["type"] = (category.group(1) if category else "") + ("(" + sub.group(1) + ")" if sub and sub.group(1) else "") + ("" if not container or container.group(1) == "None" else " [" + container.group(1) + "]")
            try:
                _CTX["side"] = "child"
                _trace(step="read_child", i=index, name=plain)
                value = cdo.get_editor_property(plain)
                entry["python_type"] = type(value).__name__
                _trace(step="convert_child", i=index, name=plain, python_type=entry["python_type"], depth=0)
                entry["value"] = convert(value)
                readable += 1
            except Exception as exc:
                entry["error"] = type(exc).__name__
                unreadable.append(plain)
                rows.append(entry)
                continue
            if parent_cdo is not None:
                try:
                    _CTX["side"] = "parent"
                    _trace(step="read_parent", i=index, name=plain)
                    parent_raw = parent_cdo.get_editor_property(plain)
                    _trace(step="convert_parent", i=index, name=plain, python_type=type(parent_raw).__name__, depth=0)
                    pv = convert(parent_raw)
                    entry["differs_from_parent"] = pv != entry["value"]
                    if entry["differs_from_parent"]:
                        entry["parent_value"] = pv
                except Exception:
                    entry["differs_from_parent"] = None
            rows.append(entry)
        if tracing:
            _trace(step="done", members=len(names), readable=readable)
        result["member_count"] = len(names)
        result["readable_count"] = readable
        result["unreadable_count"] = len(unreadable)
        result["unreadable_sample"] = unreadable[:12]
        result["differing"] = [r for r in rows if r.get("differs_from_parent")]
        wanted = [w.lower() for w in _PARAMS.get("find", [])]
        result["wanted"] = [r for r in rows if r["name"].lower() in wanted]
        result["types_seen"] = sorted(set(r.get("type", "?") for r in rows))[:40]

    if "trace" in tasks:
        # Chaque étape écrit sa trace dans le journal AVANT l'opération (print -> LogPython), pour
        # que le dernier message identifie l'opération en cause si l'éditeur plante.
        phases = _PARAMS.get("phases", ["enumerate", "values", "parent"])
        limit = int(_PARAMS.get("limit", 100000))
        offset = int(_PARAMS.get("offset", 0))
        clock = time.time
        started = clock()
        timings = {}
        qualified_names = _try(warnings, "names", lambda: [str(n) for n in blueprint.list_member_variable_names()], [])
        print("CONAN_TRACE names count=" + str(len(qualified_names)))
        plain_names = [q.rsplit(".", 1)[-1] for q in qualified_names]
        if "enumerate" in phases:
            t0 = clock()
            kinds = {}
            for i, qualified in enumerate(qualified_names[offset:offset + limit], offset):
                print("CONAN_TRACE enumerate.type " + str(i) + " " + qualified)
                pin = _try(warnings, "type:" + qualified, lambda: blueprint.get_member_variable_type(qualified))
                print("CONAN_TRACE enumerate.export " + str(i) + " " + qualified)
                if pin is not None:
                    kinds[qualified] = _try(warnings, "export:" + qualified, lambda: pin.export_text()[:60])
            timings["enumerate_s"] = round(clock() - t0, 3)
            print("CONAN_TRACE enumerate.done " + str(timings["enumerate_s"]))
        if "values" in phases:
            t0 = clock()
            ok = 0
            bad = []
            for i, name in enumerate(plain_names[offset:offset + limit], offset):
                print("CONAN_TRACE values.read " + str(i) + " " + name)
                try:
                    cdo.get_editor_property(name)
                    ok += 1
                except Exception as exc:
                    bad.append([name, type(exc).__name__ + ": " + str(exc)[:80]])
            timings["values_s"] = round(clock() - t0, 3)
            result["values_ok"] = ok
            result["values_unreadable"] = bad
            print("CONAN_TRACE values.done " + str(timings["values_s"]))
        if "plain" in phases:
            # Conversion des valeurs en JSON, étape par étape, avec trace avant chaque opération.
            t0 = clock()
            converted = 0
            for i, name in enumerate(plain_names[offset:offset + limit], offset):
                try:
                    value = cdo.get_editor_property(name)
                except Exception:
                    continue
                kind = type(value).__name__
                print("CONAN_TRACE plain.begin " + str(i) + " " + name + " " + kind)
                if value is None or isinstance(value, (bool, int, float, str)):
                    converted += 1
                    continue
                print("CONAN_TRACE plain.hasattr " + str(i) + " " + name)
                has_enum = hasattr(value, "name") and hasattr(value, "value")
                print("CONAN_TRACE plain.path " + str(i) + " " + name)
                has_path = hasattr(value, "get_path_name")
                if has_path:
                    print("CONAN_TRACE plain.call_path " + str(i) + " " + name)
                    value.get_path_name()
                elif isinstance(value, (list, tuple)) or kind in ("Array", "Set", "Map"):
                    print("CONAN_TRACE plain.container " + str(i) + " " + name)
                    list(value)[:20]
                else:
                    print("CONAN_TRACE plain.str " + str(i) + " " + name)
                    str(value)
                converted += 1
            timings["plain_s"] = round(clock() - t0, 3)
            result["plain_converted"] = converted
            print("CONAN_TRACE plain.done " + str(timings["plain_s"]))
        if "parent" in phases:
            t0 = clock()
            asset_data = registry.get_asset_by_object_path(object_path)
            parent_tag = _quoted(_try(warnings, "tag ParentClass", lambda: asset_data.get_tag_value("ParentClass"))) if asset_data is not None and asset_data.is_valid() else None
            result["parent_class"] = parent_tag
            print("CONAN_TRACE parent.load_class " + str(parent_tag))
            parent_class = unreal.load_object(None, parent_tag) if parent_tag else None
            print("CONAN_TRACE parent.cdo")
            parent_cdo = unreal.get_default_object(parent_class) if parent_class is not None else None
            ok = 0
            bad = []
            if parent_cdo is not None:
                for i, name in enumerate(plain_names[offset:offset + limit], offset):
                    print("CONAN_TRACE parent.read " + str(i) + " " + name)
                    try:
                        parent_cdo.get_editor_property(name)
                        ok += 1
                    except Exception as exc:
                        bad.append(name)
            timings["parent_s"] = round(clock() - t0, 3)
            result["parent_ok"] = ok
            result["parent_unreadable_count"] = len(bad)
            print("CONAN_TRACE parent.done " + str(timings["parent_s"]))
        result["timings"] = timings
        result["member_count"] = len(qualified_names)

    if "names" in tasks:
        # Énumération seule: aucun get_editor_property, aucune conversion.
        result["qualified_names"] = _try(warnings, "names", lambda: [str(n) for n in blueprint.list_member_variable_names()], [])

    if "read_state" in tasks:
        # Lecture complète des défauts (même conversion que l'inventaire), packages sales avant/après.
        result["dirty_before"] = _try(warnings, "dirty before", _dirty_packages)
        names = _try(warnings, "names", lambda: [str(n) for n in blueprint.list_member_variable_names()], [])
        converted = 0
        unreadable = 0
        for qualified in names:
            plain = qualified.rsplit(".", 1)[-1]
            try:
                value = cdo.get_editor_property(plain)
            except Exception:
                unreadable += 1
                continue
            _plain(value)
            converted += 1
        result["members"] = len(names)
        result["converted"] = converted
        result["unreadable"] = unreadable
        result["dirty_after"] = _try(warnings, "dirty after", _dirty_packages)

    if "paths" in tasks:
        paths = {}
        for label, call in (
            ("system_path_of_asset", lambda: unreal.SystemLibrary.get_system_path(blueprint)),
            ("project_dir", lambda: unreal.Paths.project_dir()),
            ("project_content_dir", lambda: unreal.Paths.project_content_dir()),
            ("project_mods_dir", lambda: unreal.Paths.project_mods_dir()),
            ("project_content_directory", lambda: unreal.SystemLibrary.get_project_content_directory()),
            ("project_saved_directory", lambda: unreal.SystemLibrary.get_project_saved_directory()),
            ("full_mods", lambda: unreal.Paths.convert_relative_path_to_full(unreal.Paths.project_mods_dir())),
            ("package_path_name", lambda: blueprint.get_package().get_path_name()),
            ("outermost", lambda: blueprint.get_outermost().get_path_name()),
        ):
            paths[label] = _try(warnings, label, lambda: _plain(call()))
        result["paths"] = paths

    if "try_get" in tasks:
        tries = {}
        for name in _PARAMS.get("names", []):
            try:
                value = cdo.get_editor_property(name)
                tries[name] = {"python_type": type(value).__name__, "value": _plain(value)}
            except Exception as exc:
                tries[name] = {"error": type(exc).__name__ + ": " + str(exc)[:90]}
        result["try_get"] = tries

    if "subobjects" in tasks:
        library = unreal.SubobjectDataBlueprintFunctionLibrary
        subsystem = _try(warnings, "subsystem", lambda: unreal.get_engine_subsystem(unreal.SubobjectDataSubsystem))
        handles = _try(warnings, "gather", lambda: subsystem.k2_gather_subobject_data_for_blueprint(blueprint), [])
        items = []
        for handle in handles or []:
            item = {}
            data = _try(warnings, "get_data", lambda: library.get_data(handle))
            item["display_name"] = _try(warnings, "display_name", lambda: str(library.get_display_name(data)))
            item["variable_name"] = _try(warnings, "variable_name", lambda: str(library.get_variable_name(data)))
            item["is_component"] = _try(warnings, "is_component", lambda: bool(library.is_component(data)))
            item["is_inherited"] = _try(warnings, "is_inherited", lambda: bool(library.is_inherited_component(data)))
            item["is_native"] = _try(warnings, "is_native", lambda: bool(library.is_native_component(data)))
            obj = _try(warnings, "get_object_for_blueprint", lambda: library.get_object_for_blueprint(data, blueprint))
            if obj is None:
                obj = _try(warnings, "get_object", lambda: library.get_object(data, True))
            item["object"] = _plain(obj)
            item["object_class"] = type(obj).__name__ if obj is not None else None
            if obj is not None and _PARAMS.get("component_props"):
                props = {}
                for name in _public_values(warnings, obj):
                    try:
                        props[name] = _plain(obj.get_editor_property(name))
                    except Exception:
                        pass
                item["property_count"] = len(props)
                needles = [n.lower() for n in _PARAMS.get("component_find", [])]
                item["properties"] = {k: v for k, v in props.items() if not needles or any(n in k.lower() for n in needles)}
            items.append(item)
        result["subobjects"] = items

    _emit(result)


_main()
"##;

/// Sonde de compilation (SANS enregistrement): packages sales avant/après `compile_blueprint`.
/// La compilation modifie l'état mémoire du Blueprint (il peut devenir « modifié »); rien n'est
/// enregistré sur disque. Gabarit séparé pour que l'exception reste visible et testée.
const PROBE_COMPILE_BODY: &str = r##"
def _dirty_packages():
    utils = unreal.EditorLoadingAndSavingUtils
    names = []
    for getter in (utils.get_dirty_content_packages, utils.get_dirty_map_packages):
        names.extend(str(package.get_name()) for package in getter())
    return sorted(set(names))


def _main():
    warnings = []
    object_path = _PARAMS["object_path"]
    blueprint = unreal.load_asset(object_path)
    if blueprint is None:
        _emit({"error": "Asset introuvable ou non chargeable: " + object_path})
        return
    result = {"object_path": object_path, "warnings": warnings}
    result["dirty_before"] = _try(warnings, "dirty before", _dirty_packages)
    result["status_before"] = _try(warnings, "status before", lambda: str(blueprint.get_editor_property("status")))
    unreal.BlueprintEditorLibrary.compile_blueprint(blueprint)
    result["dirty_after"] = _try(warnings, "dirty after", _dirty_packages)
    result["status_after"] = _try(warnings, "status after", lambda: str(blueprint.get_editor_property("status")))
    _emit(result)


_main()
"##;

pub const PROBE: Template = Template::new("probe", PROBE_BODY);
pub const PROBE_COMPILE: Template = Template::new("probe_compile", PROBE_COMPILE_BODY);

pub const ALL: [Template; 2] = [PROBE, PROBE_COMPILE];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ue_bridge::templates::tests::assert_read_only_except;

    #[test]
    fn probes_are_read_only_and_valid_python() {
        // Sonde de compilation: aucune trace de fichier, aucun enregistrement; seule exception: compile.
        let compile = PROBE_COMPILE.source();
        assert_read_only_except(&PROBE_COMPILE, &["compile_blueprint"]);
        assert!(compile.contains("compile_blueprint(blueprint)"));
        assert_eq!(compile.matches("compile_blueprint").count(), 1);
        assert!(!compile
            .to_lowercase()
            .replace("editorloadingandsavingutils", "")
            .contains("sav"));
        assert!(!compile.contains("open(") && !compile.contains("fsync"));
        {
            let p = PROBE;
            // Seule exception: la trace durable (fichier .log sous D:/ConanMod/, fsync).
            assert_read_only_except(&p, &["open(", ".write(", "os."]);
            let src = p.source();
            assert_eq!(
                src.matches("= open(").count(),
                1,
                "une seule ouverture de fichier: la trace"
            );
            assert_eq!(src.matches(".write(").count(), 1);
            assert!(
                src.contains("path.startswith(\"D:/ConanMod/\")")
                    && src.contains("\"/Mods/\" in path")
            );
            assert!(src.contains("os.fsync(") && src.contains(".flush()"));
            assert_eq!(
                p.source().matches("load_asset(").count(),
                1,
                "un seul chargement ciblé"
            );
        }
        let Some(py) = ["python3", "python"].iter().find(|c| {
            std::process::Command::new(c)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        }) else {
            eprintln!("SKIPPED probes_are_read_only_and_valid_python: python3 absent");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("probe.py");
        std::fs::write(
            &file,
            PROBE.render(&serde_json::json!({"object_path": "/Game/X.X", "tasks": []})),
        )
        .unwrap();
        let out = std::process::Command::new(py)
            .args(["-I", "-c", "import sys; compile(open(sys.argv[1], encoding='utf-8').read(), sys.argv[1], 'exec')"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Faux module `unreal` minimal pour exécuter la tâche `inventory` hors éditeur.
    const FAKE_UNREAL: &str = r##"
import sys, types, os
unreal = types.ModuleType("unreal")
class _Pin:
    def __init__(self, t): self.t = t
    def export_text(self): return self.t
class _Obj:
    def __init__(self, path): self.path = path
    def get_path_name(self): return self.path
class Array(list): pass
class Map:
    def __init__(self, d): self.d = d
    def __iter__(self): return iter(self.d)
    def __getitem__(self, k): return self.d[k]
class _Struct:
    def __repr__(self): return "<Struct 'S'>"
class _Cdo:
    def __init__(self, v): self.v = v
    def get_editor_property(self, n):
        if n == "Hidden": raise Exception("non lisible")
        return self.v[n]
VALUES = {"Val": 5, "Items": Array([_Obj("/Game/A.A"), _Obj("/Game/B.B")]), "Shapes": Map({"K1": Array([1, 2]), "K2": _Struct()}), "Hidden": 0}
class _Bp:
    def generated_class(self): return "GeneratedClass"
    def list_member_variable_names(self): return ["/Script/A.B.Val", "/Game/X.X_C.Items", "/Game/X.X_C.Shapes", "/Game/X.X_C.Hidden"]
    def get_member_variable_type(self, n): return _Pin('(PinCategory="int",PinSubCategory="",PinSubCategoryObject=None,ContainerType=None)')
class _Asset:
    def is_valid(self): return True
    def get_tag_value(self, n): return "/Script/Engine.BlueprintGeneratedClass'/Game/P.P_C'"
class _Registry:
    def get_asset_by_object_path(self, p): return _Asset()
class AssetRegistryHelpers:
    @staticmethod
    def get_asset_registry(): return _Registry()
def load_asset(p): return _Bp()
def load_object(outer, p): return "ParentClass"
def get_default_object(c): return _Cdo(VALUES)
for k, v in list(globals().items()):
    if not k.startswith("__") and k not in ("sys", "types", "unreal", "os"): setattr(unreal, k, v)
sys.modules["unreal"] = unreal
_FSYNCS = []
_real_fsync = os.fsync
os.fsync = lambda fd: (_FSYNCS.append(fd), _real_fsync(fd))[1]
"##;

    #[test]
    fn durable_trace_covers_members_and_container_entries_with_fsync_per_line() {
        let Some(py) = ["python3", "python"].iter().find(|c| {
            std::process::Command::new(c)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        }) else {
            eprintln!("SKIPPED durable_trace: python3 absent");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let trace = dir.path().join("probe_trace.log");
        // Le chemin réel doit être sous D:/ConanMod/; le test substitue le préfixe par un dossier temporaire.
        let prefix = format!("{}/", dir.path().display());
        let params = serde_json::json!({
            "object_path": "/Game/X.X", "tasks": ["inventory"], "find": ["Val"],
            "trace_path": trace.display().to_string(),
        });
        let source = PROBE
            .render(&params)
            .replace("\"D:/ConanMod/\"", &format!("{prefix:?}"));
        let script = dir.path().join("run.py");
        std::fs::write(
            &script,
            format!("{FAKE_UNREAL}\n{source}\nprint(\"FSYNCS=\" + str(len(_FSYNCS)))\n"),
        )
        .unwrap();
        let out = std::process::Command::new(py)
            .arg("-I")
            .arg(&script)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        // une ligne = un fsync
        assert!(
            stdout.contains(&format!("FSYNCS={}", lines.len())),
            "{stdout}"
        );
        let steps: Vec<&str> = lines.iter().map(|l| l["step"].as_str().unwrap()).collect();
        assert_eq!(steps[0], "start");
        assert_eq!(*steps.last().unwrap(), "done");
        for needed in [
            "member_begin",
            "member_typed",
            "read_child",
            "convert_child",
            "read_parent",
            "convert_parent",
            "enter",
            "list_begin",
            "list_done",
            "element",
            "map_begin",
            "map_key",
            "map_lookup",
            "map_value",
            "get_path_name",
            "str",
        ] {
            assert!(steps.contains(&needed), "étape {needed} absente: {steps:?}");
        }
        // chaque conversion est tracée AVANT: pour le membre Shapes, la clé précède sa valeur
        let pos = |step: &str, key: &str| {
            lines
                .iter()
                .position(|l| l["step"] == step && l["name"] == "Shapes" && l["key"] == key)
        };
        let lookup = pos("map_lookup", "K1").unwrap();
        let value = lines
            .iter()
            .position(|l| l["step"] == "map_value" && l["name"] == "Shapes" && l["index"] == 0)
            .unwrap();
        assert!(lookup < value);
        // le contexte de membre (index, nom, propriétaire, type exporté, côté) est sur les lignes de conversion
        let elem = lines
            .iter()
            .find(|l| l["step"] == "element" && l["name"] == "Items")
            .unwrap();
        assert_eq!(elem["owner"], "/Game/X.X_C");
        assert!(elem["type"]
            .as_str()
            .unwrap()
            .contains("PinCategory=\"int\""));
        assert_eq!(
            (elem["side"].as_str(), elem["elem_type"].as_str()),
            (Some("child"), Some("_Obj"))
        );
        assert!(lines
            .iter()
            .any(|l| l["side"] == "parent" && l["step"] == "element"));
    }

    #[test]
    fn the_trace_path_is_confined_to_d_conanmod_log_files() {
        let Some(py) = ["python3", "python"].iter().find(|c| {
            std::process::Command::new(c)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        }) else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        for bad in [
            "/tmp/x.log",
            "D:/ConanMod/../x.log",
            "D:/ConanMod/x.txt",
            "D:/Program Files/CEUE5Devkit/UE4/Content/Mods/test/x.log",
            "D:/ConanMod/Mods/x.log",
            "D:/ConanMod/csv/x.log",
        ] {
            let params = serde_json::json!({"object_path": "/Game/X.X", "tasks": ["inventory"], "trace_path": bad});
            let script = dir.path().join("bad.py");
            std::fs::write(
                &script,
                format!("{FAKE_UNREAL}\n{}\n", PROBE.render(&params)),
            )
            .unwrap();
            let out = std::process::Command::new(py)
                .arg("-I")
                .arg(&script)
                .output()
                .unwrap();
            assert!(!out.status.success(), "{bad} aurait dû être refusé");
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("trace_path refusé"),
                "{bad}"
            );
        }
        assert!(!std::path::Path::new("D:").exists());
    }
}
