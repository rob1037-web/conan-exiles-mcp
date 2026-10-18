//! Sondes de la Phase 4b (étape 1): introspection **en lecture seule** du Devkit pour établir ce
//! que Python peut lire sur les valeurs par défaut des Blueprints et de leurs composants.
//!
//! Elles ne sont jamais exposées par un outil MCP et ne sont pas dans le binaire livré (feature
//! `test-support`, comme le faux éditeur); seul l'exemple `ue_spike` les lance. Mêmes interdits que les
//! gabarits des outils (voir `templates`): aucune écriture, aucun save/compile/set.

use super::templates::Template;

const PROBE_BODY: &str = r##"
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
        for index, qualified in enumerate(names):
            if trace_members:
                print("CONAN_TRACE inventory.member " + str(index) + " " + qualified)
            plain = qualified.rsplit(".", 1)[-1]
            owner = qualified[: -(len(plain) + 1)] if "." in qualified else ""
            entry = {"name": plain, "owner": owner, "in_python_dir": plain in python_names or _snake_guess(plain) in python_names}
            pin = _try(warnings, "type:" + qualified, lambda: blueprint.get_member_variable_type(qualified).export_text())
            if pin:
                category = re.search(r'PinCategory="([^"]*)"', pin)
                sub = re.search(r'PinSubCategory="([^"]*)"', pin)
                container = re.search(r'ContainerType=(\w+)', pin)
                entry["type"] = (category.group(1) if category else "") + ("(" + sub.group(1) + ")" if sub and sub.group(1) else "") + ("" if not container or container.group(1) == "None" else " [" + container.group(1) + "]")
            try:
                value = cdo.get_editor_property(plain)
                entry["python_type"] = type(value).__name__
                entry["value"] = _plain(value)
                readable += 1
            except Exception as exc:
                entry["error"] = type(exc).__name__
                unreadable.append(plain)
                rows.append(entry)
                continue
            if parent_cdo is not None:
                try:
                    pv = _plain(parent_cdo.get_editor_property(plain))
                    entry["differs_from_parent"] = pv != entry["value"]
                    if entry["differs_from_parent"]:
                        entry["parent_value"] = pv
                except Exception:
                    entry["differs_from_parent"] = None
            rows.append(entry)
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

pub const PROBE: Template = Template::new("probe", PROBE_BODY);

pub const ALL: [Template; 1] = [PROBE];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ue_bridge::templates::tests::assert_read_only;

    #[test]
    fn probes_are_read_only_and_valid_python() {
        for p in ALL {
            assert_read_only(&p);
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
}
