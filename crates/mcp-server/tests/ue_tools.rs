//! Outils ue_* (Phase 4a, lecture seule) contre un FAUX éditeur local: aucun test ne dépend du
//! Devkit réel.

mod common;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use common::*;
use mcp_server::ue_bridge::templates::{self, MARKER_BEGIN, MARKER_END};
use mcp_server::ue_bridge::testing::{Behavior, FakeNode, Reply};
use serde_json::{json, Value};

/// Le faux éditeur répond en imprimant `payload` entre les marqueurs.
fn node_replying(payload: Value) -> FakeNode {
    FakeNode::start(Behavior::default(), move |_| {
        Reply::printing(&format!("{MARKER_BEGIN}{payload}{MARKER_END}"))
    })
}

async fn session(node: &FakeNode) -> (Env, Session) {
    let env = Env::new();
    let s = connect_with_bridge(&env.config(), node.config()).await;
    (env, s)
}

/// Le texte de la réponse est le même JSON compact que le contenu structuré (une seule ligne).
fn assert_json_text(r: &rmcp::model::CallToolResult) {
    let t = text(r);
    assert!(!t.contains('\n'), "JSON compact attendu: {t}");
    assert_eq!(
        &serde_json::from_str::<Value>(&t).expect("texte JSON"),
        structured(r)
    );
}

/// Paramètres que le script a reçus: ce qui est dans le littéral de `_PARAMS = json.loads("...")`.
fn params_of(command: &str) -> Value {
    let line = command
        .lines()
        .find(|l| l.starts_with("_PARAMS = json.loads("))
        .expect("ligne _PARAMS");
    let literal = line
        .trim_start_matches("_PARAMS = json.loads(")
        .trim_end_matches(')');
    let text: String = serde_json::from_str(literal).expect("littéral de chaîne");
    serde_json::from_str(&text).expect("JSON des paramètres")
}

#[tokio::test]
async fn the_six_ue_tools_are_exposed_as_read_only() {
    let node = node_replying(json!({}));
    let (_env, s) = session(&node).await;
    let tools = s.client.list_all_tools().await.unwrap();
    for name in [
        "ue_status",
        "ue_search_assets",
        "ue_describe_asset",
        "ue_blueprint_variables",
        "ue_get_defaults",
        "ue_diff_defaults",
    ] {
        let t = tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name} absent"));
        assert!(
            t.description.as_deref().unwrap().contains("LECTURE SEULE"),
            "{name}"
        );
    }
    let search = tools.iter().find(|t| t.name == "ue_search_assets").unwrap();
    let schema = serde_json::to_string(&search.input_schema).unwrap();
    assert!(["path_prefix", "class_name", "name_contains", "limit"]
        .iter()
        .all(|k| schema.contains(k)));
}

#[tokio::test]
async fn ue_status_reports_the_editor() {
    let node = node_replying(json!({
        "engine_version": "5.8.2-test", "project_name": "ConanSandbox", "project_dir": "D:/Fake/",
        "asset_count": 85240, "registry_loading": false, "python_version": "3.11.8", "warnings": []
    }));
    let (_env, s) = session(&node).await;
    let r = s.call("ue_status", json!({})).await;
    assert!(!is_error(&r), "{}", text(&r));
    assert_json_text(&r);
    let v = structured(&r);
    assert_eq!(v["reachable"], true);
    assert_eq!(v["asset_count"], 85240);
    assert_eq!(v["engine_version"], "5.8.2-test");
    assert_eq!(v["node"]["project_name"], "ConanSandbox");
    // une session propre: une ouverture, une fermeture
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert_eq!(
        (
            node.opens.load(Ordering::SeqCst),
            node.closes.load(Ordering::SeqCst)
        ),
        (1, 1)
    );
}

#[tokio::test]
async fn ue_status_with_the_devkit_closed_is_a_normal_unreachable_answer() {
    let env = Env::new();
    let s = connect_with_bridge(&env.config(), FakeNode::absent_config()).await;
    let r = s.call("ue_status", json!({})).await;
    assert!(!is_error(&r), "{}", text(&r));
    assert_json_text(&r);
    assert_eq!(structured(&r)["reachable"], false);
    assert_eq!(structured(&r)["error_code"], "DevkitNotRunning");
    let message = structured(&r)["error"].as_str().unwrap();
    assert!(
        message.starts_with("aucun éditeur Unreal n'a répondu"),
        "{message}"
    );
    assert!(
        !text(&r).contains("INJOIGNABLE") && !text(&r).to_lowercase().contains("injoignable"),
        "{}",
        text(&r)
    );
    // les autres outils, eux, signalent une erreur d'outil
    for (tool, args) in [
        ("ue_search_assets", json!({})),
        ("ue_describe_asset", json!({"object_path": "/Game/X/Y"})),
        (
            "ue_blueprint_variables",
            json!({"object_path": "/Game/X/Y"}),
        ),
    ] {
        let r = s.call(tool, args).await;
        assert!(
            is_error(&r) && text(&r).starts_with("ERREUR DevkitNotRunning:"),
            "{tool}: {}",
            text(&r)
        );
    }
}

#[tokio::test]
async fn ue_search_assets_passes_json_parameters_and_formats_the_answer() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen2 = seen.clone();
    let payload = json!({
        "total": 899, "returned": 2, "prefixes_tried": [], "warnings": [],
        "assets": [
            {"object_path": "/Game/Items/ItemTable.ItemTable", "class": "DataTable", "class_package": "/Script/Engine", "package": "/Game/Items/ItemTable", "name": "ItemTable"},
            {"object_path": "/Game/Systems/DT_X.DT_X", "class": "DataTable", "class_package": "/Script/Engine", "package": "/Game/Systems/DT_X", "name": "DT_X"}
        ]
    });
    let node = FakeNode::start(Behavior::default(), move |cmd| {
        seen2.lock().unwrap().push(cmd.to_owned());
        Reply::printing(&format!("{MARKER_BEGIN}{payload}{MARKER_END}"))
    });
    let (_env, s) = session(&node).await;

    let r = s
        .call(
            "ue_search_assets",
            json!({"class_name": "DataTable", "limit": 2}),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert_json_text(&r);
    assert_eq!(structured(&r)["total"], 899);
    assert_eq!(structured(&r)["truncated"], true);

    // paramètres reçus par le script, défauts compris
    s.call("ue_search_assets", json!({})).await;
    s.call(
        "ue_search_assets",
        json!({"limit": 9999, "path_prefix": " Mods/test "}),
    )
    .await;
    let cmds = seen.lock().unwrap().clone();
    let p0 = params_of(&cmds[0]);
    assert_eq!(
        (p0["class_name"].as_str(), p0["limit"].as_u64()),
        (Some("DataTable"), Some(2))
    );
    assert!(p0["path_prefix"].is_null());
    assert_eq!(params_of(&cmds[1])["limit"], 50, "défaut 50");
    let p2 = params_of(&cmds[2]);
    assert_eq!(
        (p2["limit"].as_u64(), p2["path_prefix"].as_str()),
        (Some(500), Some("Mods/test")),
        "maximum 500, espaces retirés"
    );
}

#[tokio::test]
async fn ue_search_assets_rejects_bad_parameters_without_contacting_the_editor() {
    let node = node_replying(json!({"total": 0, "assets": []}));
    let (_env, s) = session(&node).await;
    let r = s.call("ue_search_assets", json!({"limit": 0})).await;
    assert!(
        is_error(&r) && text(&r).starts_with("ERREUR InvalidParameter:"),
        "{}",
        text(&r)
    );
    let long = "x".repeat(600);
    let r = s
        .call("ue_search_assets", json!({"name_contains": long}))
        .await;
    assert!(
        is_error(&r) && text(&r).contains("InvalidParameter"),
        "{}",
        text(&r)
    );
    assert!(node.commands.lock().unwrap().is_empty());
    assert_eq!(node.opens.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn hostile_parameters_stay_inside_the_json_literal() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen2 = seen.clone();
    let node = FakeNode::start(Behavior::default(), move |cmd| {
        seen2.lock().unwrap().push(cmd.to_owned());
        Reply::printing(&format!(
            "{MARKER_BEGIN}{{\"total\": 0, \"assets\": [], \"warnings\": []}}{MARKER_END}"
        ))
    });
    let (_env, s) = session(&node).await;
    let nasty =
        "x\"); import os; os.system('rm -rf /') #\n''' + __import__('os') + '''é\u{2028}fin";
    let r = s
        .call("ue_search_assets", json!({"name_contains": nasty}))
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    let cmd = seen.lock().unwrap()[0].clone();
    assert_eq!(params_of(&cmd)["name_contains"], nasty);
    // hors de la ligne des paramètres, le script est exactement le gabarit figé
    let line = cmd
        .lines()
        .find(|l| l.starts_with("_PARAMS = json.loads("))
        .unwrap()
        .to_owned();
    let template_src = templates::SEARCH_ASSETS.source();
    assert_eq!(
        cmd.replace(&line, ""),
        template_src.replace("_PARAMS = json.loads(__CONAN_PARAMS__)", "")
    );
}

#[tokio::test]
async fn ue_describe_asset_normalizes_the_path_and_formats_tags_and_dependencies() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen2 = seen.clone();
    let payload = json!({
        "object_path": "/Game/Mods/test/test/Mailbox/SM_BoiteAuxLettres.SM_BoiteAuxLettres",
        "name": "SM_BoiteAuxLettres", "package": "/Game/Mods/test/test/Mailbox/SM_BoiteAuxLettres",
        "package_path": "/Game/Mods/test/test/Mailbox", "class": "StaticMesh", "class_package": "/Script/Engine",
        "tags": {"Triangles": "224", "Vertices": "492"},
        "dependency_count": 2, "dependencies": ["/Script/NavigationSystem", "/Game/Items/MI_X"], "warnings": []
    });
    let node = FakeNode::start(Behavior::default(), move |cmd| {
        seen2.lock().unwrap().push(cmd.to_owned());
        Reply::printing(&format!("{MARKER_BEGIN}{payload}{MARKER_END}"))
    });
    let (_env, s) = session(&node).await;
    // sans suffixe: ajouté
    let r = s
        .call(
            "ue_describe_asset",
            json!({"object_path": "/Game/Mods/test/test/Mailbox/SM_BoiteAuxLettres"}),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert_json_text(&r);
    let v = structured(&r);
    assert_eq!(v["class"], "StaticMesh");
    assert_eq!(v["tags"]["Triangles"], "224");
    assert_eq!(v["dependency_count"], 2);
    assert!(v["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d == "/Game/Items/MI_X"));
    assert!(
        v.get("asset_members").is_none(),
        "pas de clé nulle de diagnostic"
    );
    assert_eq!(
        params_of(&seen.lock().unwrap()[0])["object_path"],
        "/Game/Mods/test/test/Mailbox/SM_BoiteAuxLettres.SM_BoiteAuxLettres"
    );
    assert_eq!(structured(&r)["class"], "StaticMesh");
    // déjà complet: inchangé
    s.call("ue_describe_asset", json!({"object_path": "/Game/A/B.B"}))
        .await;
    assert_eq!(
        params_of(&seen.lock().unwrap()[1])["object_path"],
        "/Game/A/B.B"
    );
}

#[tokio::test]
async fn ue_describe_asset_refuses_malformed_paths_and_reports_missing_assets() {
    let node =
        node_replying(json!({"error": "Asset introuvable dans l'Asset Registry: /Game/Nope.Nope"}));
    let (_env, s) = session(&node).await;
    for bad in [
        "",
        "Game/X",
        "/Game/X Y",
        "/Game/../X",
        "/Game/X\nY",
        "/Game/X'",
        "/Game/",
    ] {
        let r = s
            .call("ue_describe_asset", json!({"object_path": bad}))
            .await;
        assert!(
            is_error(&r) && text(&r).starts_with("ERREUR InvalidParameter:"),
            "{bad:?}: {}",
            text(&r)
        );
    }
    assert!(
        node.commands.lock().unwrap().is_empty(),
        "rien n'est envoyé à l'éditeur"
    );
    let r = s
        .call("ue_describe_asset", json!({"object_path": "/Game/Nope"}))
        .await;
    assert!(
        is_error(&r)
            && text(&r).starts_with("ERREUR PythonError:")
            && text(&r).contains("introuvable"),
        "{}",
        text(&r)
    );
}

#[tokio::test]
async fn ue_blueprint_variables_formats_variables_and_passes_include_inherited() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen2 = seen.clone();
    let payload = json!({
        "object_path": "/Game/X/BP_X.BP_X", "own_variable_count": 2, "inherited_variable_count": 121,
        "variables": [
            {"name": "MaxAmount", "inherited": false, "category": "Items", "replication": "BlueprintVariableReplication.NONE",
             "type": {"label": "int", "category": "int", "sub_category": "", "sub_category_object": null, "container": "None"},
             "default_available": true, "default": 1, "default_python_type": "int"},
            {"name": "PossibleItemIDs", "inherited": false, "category": "Items", "replication": "BlueprintVariableReplication.NONE",
             "type": {"label": "Array of int"}, "default_available": true, "default": [11103], "default_python_type": "Array"}
        ],
        "warnings": []
    });
    let node = FakeNode::start(Behavior::default(), move |cmd| {
        seen2.lock().unwrap().push(cmd.to_owned());
        Reply::printing(&format!("{MARKER_BEGIN}{payload}{MARKER_END}"))
    });
    let (_env, s) = session(&node).await;
    let r = s
        .call(
            "ue_blueprint_variables",
            json!({"object_path": "/Game/X/BP_X"}),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert_json_text(&r);
    let v = structured(&r);
    assert_eq!(v["variables"][0]["name"], "MaxAmount");
    assert_eq!(v["variables"][1]["type"]["label"], "Array of int");
    assert_eq!(v["variables"][1]["default"], json!([11103]));
    assert_eq!(v["inherited_variable_count"], 121);
    assert_eq!(structured(&r)["own_variable_count"], 2);
    s.call(
        "ue_blueprint_variables",
        json!({"object_path": "/Game/X/BP_X.BP_X", "include_inherited": true}),
    )
    .await;
    let cmds = seen.lock().unwrap().clone();
    assert_eq!(params_of(&cmds[0])["include_inherited"], false);
    assert_eq!(params_of(&cmds[0])["object_path"], "/Game/X/BP_X.BP_X");
    assert_eq!(params_of(&cmds[1])["include_inherited"], true);
}

#[tokio::test]
async fn python_timeout_and_protocol_failures_are_coded_tool_errors() {
    // traceback Python
    let tb = "Traceback (most recent call last):\n  File \"<string>\", line 9, in <module>\nAttributeError: boom";
    let node = FakeNode::start(Behavior::default(), move |_| Reply::PythonError {
        traceback: tb.into(),
    });
    let (_env, s) = session(&node).await;
    let r = s.call("ue_search_assets", json!({})).await;
    assert!(
        is_error(&r)
            && text(&r).starts_with("ERREUR PythonError:")
            && text(&r).contains("AttributeError: boom")
            && text(&r).contains("lecture seule"),
        "{}",
        text(&r)
    );
    // délai dépassé
    let node = FakeNode::start(Behavior::default(), |_| Reply::Silence);
    let mut cfg = node.config();
    cfg.command_timeout = std::time::Duration::from_millis(300);
    let env = Env::new();
    let s = connect_with_bridge(&env.config(), cfg).await;
    let r = s.call("ue_search_assets", json!({})).await;
    assert!(
        is_error(&r) && text(&r).starts_with("ERREUR Timeout:"),
        "{}",
        text(&r)
    );
    // protocole
    for reply in [
        Reply::Garbage,
        Reply::WrongType,
        Reply::Hangup,
        Reply::printing("pas de marqueurs"),
    ] {
        let node = FakeNode::start(Behavior::default(), move |_| reply.clone());
        let env = Env::new();
        let s = connect_with_bridge(&env.config(), node.config()).await;
        let r = s.call("ue_status", json!({})).await;
        assert!(
            is_error(&r) && text(&r).starts_with("ERREUR ProtocolError:"),
            "{}",
            text(&r)
        );
    }
}

#[tokio::test]
async fn the_existing_tools_keep_working_next_to_the_bridge() {
    let node = node_replying(json!({"total": 0, "assets": [], "warnings": []}));
    let (env, s) = session(&node).await;
    let r = s.call("list_mod_files", json!({"source": "mod"})).await;
    assert!(!is_error(&r), "{}", text(&r));
    let r = s.call("list_schemas", json!({})).await;
    assert_eq!(structured(&r)["count"], 1);
    let r = s
        .call("write_datatable", write_args(TABLE, CSV_V1, None, None))
        .await;
    assert!(text(&r).starts_with("DRY-RUN"), "{}", text(&r));
    assert_eq!(
        node.opens.load(Ordering::SeqCst),
        0,
        "les outils existants n'appellent jamais le Devkit"
    );
    assert_eq!(std::fs::read_dir(&env.csv_root).unwrap().count(), 0);
}

#[tokio::test]
async fn concurrent_ue_calls_are_serialized_one_session_each() {
    let node = node_replying(json!({"total": 0, "assets": [], "warnings": []}));
    let (_env, s) = session(&node).await;
    let (a, b, c) = tokio::join!(
        s.call("ue_search_assets", json!({})),
        s.call("ue_search_assets", json!({"limit": 5})),
        s.call("ue_status", json!({})),
    );
    assert!(!is_error(&a) && !is_error(&b));
    let _ = c;
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(node.opens.load(Ordering::SeqCst), 3);
    assert_eq!(node.closes.load(Ordering::SeqCst), 3);
}

// ---- Phase 4b, étape 2: ue_get_defaults / ue_diff_defaults ----

/// Faux éditeur qui sert `total` éléments « P0..Pn » par lots de 60, en lisant `start` dans les
/// paramètres que le script a reçus (comme le vrai gabarit). `value_of(asset, i)` donne la valeur.
fn defaults_node(
    total: usize,
    value_of: impl Fn(&str, usize) -> Value + Send + Sync + 'static,
) -> FakeNode {
    FakeNode::start(Behavior::default(), move |command| {
        let p = params_of(command);
        let asset = p["object_path"].as_str().unwrap().to_owned();
        let start = p["start"].as_u64().unwrap() as usize;
        let stop = (start + 60).min(total);
        let items: Vec<Value> = (start..stop)
            .map(|i| {
                json!({"name": format!("P{i}"), "owner": "/Script/X.Y", "type": "int", "value": value_of(&asset, i)})
            })
            .collect();
        let payload = json!({
            "object_path": asset, "section": p["section"], "total": total, "start": start,
            "next_index": if stop < total { json!(stop) } else { Value::Null },
            "budget_exceeded": false, "missing_names": [], "items": items,
            "dirty_packages": [], "warnings": [], "elapsed_s": 0.01,
        });
        Reply::printing(&format!("{MARKER_BEGIN}{payload}{MARKER_END}"))
    })
}

#[tokio::test]
async fn ue_get_defaults_follows_next_index_in_batches_and_returns_compact_json() {
    let node = defaults_node(130, |_, i| json!(i));
    let (_env, s) = session(&node).await;
    let r = s
        .call(
            "ue_get_defaults",
            json!({"object_path": "/Game/Mods/test/BP_Test", "names": ["P1", "P1", "P2"], "expand": ["P3"]}),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert_json_text(&r);
    let v = structured(&r);
    assert_eq!(
        (
            v["total"].clone(),
            v["returned"].clone(),
            v["complete"].clone()
        ),
        (json!(130), json!(130), json!(true))
    );
    assert_eq!(v["commands"], 3);
    assert_eq!(v["items"][129]["name"], "P129");
    assert_eq!(v["object_path"], "/Game/Mods/test/BP_Test.BP_Test");
    // les paramètres envoyés: chemin normalisé, noms dédoublonnés, `start` croissant, jamais d'écriture
    let sent: Vec<Value> = node
        .commands
        .lock()
        .unwrap()
        .iter()
        .map(|c| params_of(c))
        .collect();
    assert_eq!(sent.len(), 3);
    assert_eq!(
        sent.iter()
            .map(|p| p["start"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 60, 120]
    );
    assert_eq!(sent[0]["names"], json!(["P1", "P2"]));
    assert_eq!(sent[0]["expand"], json!(["P3"]));
    assert_eq!(sent[0]["object_path"], "/Game/Mods/test/BP_Test.BP_Test");
    for c in node.commands.lock().unwrap().iter() {
        for forbidden in ["save_", "set_editor_property", "compile_blueprint"] {
            assert!(!c.contains(forbidden), "{forbidden}");
        }
    }
}

#[tokio::test]
async fn ue_get_defaults_stops_after_12_commands_and_resumes_with_start() {
    let node = defaults_node(900, |_, i| json!(i));
    let (_env, s) = session(&node).await;
    let r = s
        .call("ue_get_defaults", json!({"object_path": "/Game/BP_Big"}))
        .await;
    let v = structured(&r);
    assert_eq!(
        (
            v["commands"].clone(),
            v["returned"].clone(),
            v["complete"].clone()
        ),
        (json!(12), json!(720), json!(false))
    );
    assert_eq!(v["next_index"], 720);
    let r = s
        .call(
            "ue_get_defaults",
            json!({"object_path": "/Game/BP_Big", "start": 720}),
        )
        .await;
    let v = structured(&r);
    assert_eq!(
        (v["returned"].clone(), v["complete"].clone()),
        (json!(180), json!(true))
    );
    assert_eq!(v["items"][0]["name"], "P720");
}

#[tokio::test]
async fn ue_get_defaults_validates_parameters_before_touching_the_editor() {
    let node = defaults_node(3, |_, i| json!(i));
    let (_env, s) = session(&node).await;
    let too_many: Vec<String> = (0..201).map(|i| format!("P{i}")).collect();
    for (args, expect) in [
        (json!({"object_path": "Game/BP"}), "object_path"),
        (
            json!({"object_path": "/Game/BP", "section": "all"}),
            "section",
        ),
        (
            json!({"object_path": "/Game/BP", "section": "component_values"}),
            "component",
        ),
        (
            json!({"object_path": "/Game/BP", "names": too_many}),
            "maximum 200",
        ),
        (
            json!({"object_path": "/Game/BP", "names": [""]}),
            "nom invalide",
        ),
        (
            json!({"object_path": "/Game/BP", "expand": (0..21).map(|i| format!("E{i}")).collect::<Vec<_>>()}),
            "maximum 20",
        ),
    ] {
        let r = s.call("ue_get_defaults", args.clone()).await;
        assert!(
            is_error(&r)
                && text(&r).starts_with("ERREUR InvalidParameter:")
                && text(&r).contains(expect),
            "{args}: {}",
            text(&r)
        );
    }
    assert_eq!(
        node.opens.load(Ordering::SeqCst),
        0,
        "rien n'est envoyé à l'éditeur"
    );
}

#[tokio::test]
async fn ue_get_defaults_relays_script_errors_and_refuses_a_stuck_cursor() {
    let node =
        node_replying(json!({"error": "Asset introuvable ou non chargeable: /Game/Nope.Nope"}));
    let (_env, s) = session(&node).await;
    let r = s
        .call("ue_get_defaults", json!({"object_path": "/Game/Nope"}))
        .await;
    assert!(
        is_error(&r)
            && text(&r).starts_with("ERREUR PythonError:")
            && text(&r).contains("introuvable"),
        "{}",
        text(&r)
    );
    // un éditeur qui répondrait toujours next_index=0 ne doit pas faire boucler le serveur
    let node = node_replying(
        json!({"object_path": "/Game/B.B", "section": "members", "total": 5, "next_index": 0, "items": [], "warnings": []}),
    );
    let (_env, s) = session(&node).await;
    let r = s
        .call("ue_get_defaults", json!({"object_path": "/Game/B"}))
        .await;
    assert!(
        is_error(&r) && text(&r).contains("ne progresse pas"),
        "{}",
        text(&r)
    );
    assert_eq!(node.opens.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ue_diff_defaults_reports_differences_with_float_tolerance() {
    let node = defaults_node(70, |asset, i| {
        let base = if i == 3 {
            json!(1000)
        } else if i == 4 {
            json!(500.0)
        } else {
            json!(i)
        };
        if asset.contains("BP_B") && i == 3 {
            json!(5000)
        } else if asset.contains("BP_B") && i == 4 {
            json!(500.0000004)
        } else {
            base
        }
    });
    let (_env, s) = session(&node).await;
    let r = s
        .call(
            "ue_diff_defaults",
            json!({"object_path_a": "/Game/BP_A", "object_path_b": "/Game/BP_B"}),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert_json_text(&r);
    let v = structured(&r);
    assert_eq!(
        (v["identical"].clone(), v["different"].clone()),
        (json!(69), json!(1))
    );
    assert_eq!(v["differences"][0]["name"], "P3");
    assert_eq!(
        (
            v["differences"][0]["a"].clone(),
            v["differences"][0]["b"].clone()
        ),
        (json!(1000), json!(5000))
    );
    assert_eq!(v["complete"], true);
    assert_eq!(v["commands"], 4);
    assert_eq!(v["a"]["object_path"], "/Game/BP_A.BP_A");
}

#[tokio::test]
async fn ue_diff_defaults_refuses_a_blueprint_too_large_to_read_in_one_call() {
    let node = defaults_node(900, |_, i| json!(i));
    let (_env, s) = session(&node).await;
    let r = s
        .call(
            "ue_diff_defaults",
            json!({"object_path_a": "/Game/BP_A", "object_path_b": "/Game/BP_B"}),
        )
        .await;
    assert!(
        is_error(&r) && text(&r).starts_with("ERREUR TooLarge:") && text(&r).contains("names"),
        "{}",
        text(&r)
    );
}

// ---- Garde-fou EditorBusy ----

fn short_timeout(node: &FakeNode) -> mcp_server::ue_bridge::BridgeConfig {
    let mut cfg = node.config();
    cfg.command_timeout = std::time::Duration::from_millis(300);
    cfg
}

/// Faux éditeur muet tant que `answer` est faux, puis répondant à `payload`.
fn node_silent_until(answer: Arc<std::sync::atomic::AtomicBool>, payload: Value) -> FakeNode {
    FakeNode::start(Behavior::default(), move |_| {
        if answer.load(Ordering::SeqCst) {
            Reply::printing(&format!("{MARKER_BEGIN}{payload}{MARKER_END}"))
        } else {
            Reply::Silence
        }
    })
}

#[tokio::test]
async fn after_a_command_timeout_ue_tools_refuse_without_sending_until_ue_status_succeeds() {
    let answer = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let node = node_silent_until(
        answer.clone(),
        json!({"engine_version": "5.8.2-test", "project_name": "ConanSandbox", "total": 0, "assets": [], "warnings": []}),
    );
    let env = Env::new();
    let s = connect_with_bridge(&env.config(), short_timeout(&node)).await;

    // 1. l'éditeur ne répond pas: Timeout, le drapeau est posé
    let r = s.call("ue_search_assets", json!({})).await;
    assert!(
        is_error(&r) && text(&r).starts_with("ERREUR Timeout:"),
        "{}",
        text(&r)
    );
    assert_eq!(node.opens.load(Ordering::SeqCst), 1);

    // 2. tous les outils ue_* sauf ue_status refusent et n'envoient RIEN (ni ping, ni connexion)
    let (opens, pings, commands) = (
        node.opens.load(Ordering::SeqCst),
        node.pings.load(Ordering::SeqCst),
        node.commands.lock().unwrap().len(),
    );
    for (tool, args) in [
        ("ue_search_assets", json!({})),
        ("ue_describe_asset", json!({"object_path": "/Game/X.X"})),
        (
            "ue_blueprint_variables",
            json!({"object_path": "/Game/X.X"}),
        ),
        ("ue_get_defaults", json!({"object_path": "/Game/X.X"})),
        (
            "ue_diff_defaults",
            json!({"object_path_a": "/Game/A.A", "object_path_b": "/Game/B.B"}),
        ),
    ] {
        let r = s.call(tool, args).await;
        let t = text(&r);
        assert!(
            is_error(&r)
                && t.starts_with("ERREUR EditorBusy:")
                && t.contains("NE RELANCE PAS")
                && t.contains("Robin"),
            "{tool}: {t}"
        );
    }
    assert_eq!(
        (
            node.opens.load(Ordering::SeqCst),
            node.pings.load(Ordering::SeqCst),
            node.commands.lock().unwrap().len()
        ),
        (opens, pings, commands),
        "aucun message envoyé à l'éditeur pendant le refus"
    );
    // les outils qui n'utilisent pas l'éditeur continuent de marcher
    let r = s.call("list_schemas", json!({})).await;
    assert!(!is_error(&r), "{}", text(&r));

    // 3. ue_status échoue (l'éditeur est toujours muet): il le dit et le drapeau reste posé
    let r = s.call("ue_status", json!({})).await;
    assert!(
        is_error(&r) && text(&r).contains("éditeur peut-être occupé (commande précédente expirée)"),
        "{}",
        text(&r)
    );
    let r = s.call("ue_search_assets", json!({})).await;
    assert!(text(&r).starts_with("ERREUR EditorBusy:"), "{}", text(&r));

    // 4. l'éditeur répond de nouveau: ue_status réussit et lève le drapeau
    answer.store(true, Ordering::SeqCst);
    let r = s.call("ue_status", json!({})).await;
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(structured(&r)["reachable"], true);
    assert_eq!(structured(&r)["busy_flag_cleared"], true);
    let r = s.call("ue_search_assets", json!({})).await;
    assert!(!is_error(&r), "{}", text(&r));
    // un ue_status ordinaire n'annonce rien
    let r = s.call("ue_status", json!({})).await;
    assert!(structured(&r).get("busy_flag_cleared").is_none());
}

#[tokio::test]
async fn a_protocol_break_after_sending_also_sets_the_flag_but_other_errors_do_not() {
    // rupture de protocole après l'envoi: drapeau
    for reply in [
        Reply::Garbage,
        Reply::Hangup,
        Reply::printing("pas de marqueurs"),
    ] {
        let node = FakeNode::start(Behavior::default(), move |_| reply.clone());
        let env = Env::new();
        let s = connect_with_bridge(&env.config(), node.config()).await;
        let r = s.call("ue_search_assets", json!({})).await;
        assert!(
            text(&r).starts_with("ERREUR ProtocolError:"),
            "{}",
            text(&r)
        );
        let r = s.call("ue_search_assets", json!({})).await;
        assert!(text(&r).starts_with("ERREUR EditorBusy:"), "{}", text(&r));
    }
    // une erreur Python (l'éditeur a répondu) ne pose pas le drapeau
    let node = FakeNode::start(Behavior::default(), |_| Reply::PythonError {
        traceback: "Traceback: boum".into(),
    });
    let env = Env::new();
    let s = connect_with_bridge(&env.config(), node.config()).await;
    for _ in 0..2 {
        let r = s.call("ue_search_assets", json!({})).await;
        assert!(text(&r).starts_with("ERREUR PythonError:"), "{}", text(&r));
    }
    // un éditeur absent (découverte) ne pose pas le drapeau
    let env = Env::new();
    let s = connect_with_bridge(&env.config(), FakeNode::absent_config()).await;
    for _ in 0..2 {
        let r = s.call("ue_search_assets", json!({})).await;
        assert!(
            text(&r).starts_with("ERREUR DevkitNotRunning:"),
            "{}",
            text(&r)
        );
    }
}

#[tokio::test]
async fn a_chained_read_stops_sending_after_the_first_timeout() {
    // ue_get_defaults enchaîne des commandes: la première expire, la suivante n'est jamais envoyée
    let node = FakeNode::start(Behavior::default(), |_| Reply::Silence);
    let env = Env::new();
    let s = connect_with_bridge(&env.config(), short_timeout(&node)).await;
    let r = s
        .call("ue_get_defaults", json!({"object_path": "/Game/BP_X"}))
        .await;
    assert!(text(&r).starts_with("ERREUR Timeout:"), "{}", text(&r));
    assert_eq!(node.commands.lock().unwrap().len(), 1);
}
