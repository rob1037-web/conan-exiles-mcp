//! Tests d'intégration client↔serveur en mémoire: chaque outil, cas nominal et cas d'erreur.
//! Les écritures passent toutes par la table réelle DT_Purge_Nordheimer et son schéma.

mod common;

use common::*;
use serde_json::json;
use std::fs;

fn put(env: &Env, rel: &str, content: &str) {
    let path = env.csv_root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

async fn write(
    s: &Session,
    content: &str,
    dry_run: Option<bool>,
    confirm: Option<bool>,
) -> rmcp::model::CallToolResult {
    s.call(
        "write_datatable",
        write_args(TABLE, content, dry_run, confirm),
    )
    .await
}

#[tokio::test]
async fn exposes_all_thirteen_tools_with_descriptions() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let tools = s.client.list_all_tools().await.unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "add_row",
            "delete_row",
            "describe_schema",
            "list_mod_files",
            "list_schemas",
            "read_datatable",
            "ue_blueprint_variables",
            "ue_describe_asset",
            "ue_search_assets",
            "ue_status",
            "update_row",
            "validate_rows",
            "write_datatable"
        ]
    );
    for t in &tools {
        assert!(
            t.description.as_deref().is_some_and(|d| d.len() > 40),
            "{}",
            t.name
        );
    }
    let write = tools.iter().find(|t| t.name == "write_datatable").unwrap();
    let schema = serde_json::to_string(&write.input_schema).unwrap();
    assert!(["table", "dry_run", "confirm", "content"]
        .iter()
        .all(|k| schema.contains(k)));
}

// --- list_mod_files ---------------------------------------------------------------

#[tokio::test]
async fn list_defaults_to_csv_root_and_can_read_the_mod_read_only() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let r = s.call("list_mod_files", json!({})).await;
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(structured(&r)["source"], "csv");
    assert_eq!(structured(&r)["count"], 0);

    let r = s.call("list_mod_files", json!({"source": "mod"})).await;
    let files: Vec<_> = structured(&r)["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(files, ["Content/Data/m.csv", "active.txt", "modinfo.json"]);
    let r = s
        .call(
            "list_mod_files",
            json!({"source": "mod", "subfolder": "Content"}),
        )
        .await;
    assert_eq!(structured(&r)["count"], 1);
}

#[tokio::test]
async fn list_never_shows_backups_or_temporaries() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    for content in [CSV_V1.to_owned(), csv_v2()] {
        let r = write(&s, &content, Some(false), Some(true)).await;
        assert!(!is_error(&r), "{}", text(&r));
    }
    assert!(env.csv_root.join(".backup").is_dir());
    fs::write(env.csv_root.join(".x.csv.tmp-1-2"), "x").unwrap();
    let r = s.call("list_mod_files", json!({})).await;
    let all = serde_json::to_string(structured(&r)).unwrap();
    assert!(!all.contains(".backup") && !all.contains(".tmp-"), "{all}");
    assert_eq!(structured(&r)["count"], 1);
    let r = s
        .call("list_mod_files", json!({"subfolder": ".backup"}))
        .await;
    assert!(
        is_error(&r) && text(&r).starts_with("ERREUR InternalFolder"),
        "{}",
        text(&r)
    );
}

#[tokio::test]
async fn list_errors_are_tool_errors() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    for (args, code) in [
        (json!({"subfolder": "../mod"}), "PathRefused"),
        (json!({"subfolder": "/etc"}), "PathRefused"),
        (json!({"subfolder": "absent"}), "NotFound"),
        (
            json!({"source": "mod", "subfolder": "..\\x"}),
            "PathRefused",
        ),
    ] {
        let r = s.call("list_mod_files", args.clone()).await;
        assert!(is_error(&r), "{args}");
        assert!(
            text(&r).starts_with(&format!("ERREUR {code}:")),
            "{args}: {}",
            text(&r)
        );
    }
}

// --- read_datatable ---------------------------------------------------------------

#[tokio::test]
async fn read_returns_columns_rows_and_format() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    write(&s, CSV_V1, Some(false), Some(true)).await;
    let r = s
        .call("read_datatable", json!({"path": "DT_Purge_Nordheimer.csv"}))
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    let v = structured(&r);
    assert_eq!(
        v["columns"],
        json!(["---", "SourceName", "TargetName", "Weight", "RowType"])
    );
    assert_eq!(v["row_count"], 5);
    assert_eq!(
        v["rows"][2],
        json!([
            "Trash3",
            "NordheimerWolf",
            "PurgeNew_Nordheimer_Wolf",
            "5.000000",
            "WeightedSpawnTable"
        ])
    );
    assert_eq!(
        v["format"],
        json!({"bom": false, "newline": "LF", "final_newline": true})
    );
}

#[tokio::test]
async fn read_paginates_and_reads_the_mod_read_only() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let args = |limit: u32, offset: u32| json!({"path": "Content/Data/m.csv", "source": "mod", "limit": limit, "offset": offset});
    let r = s.call("read_datatable", args(1, 1)).await;
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(structured(&r)["rows"][0][0], "Trash2");
    assert_eq!(structured(&r)["truncated"], true);
    let r = s.call("read_datatable", args(10, 4)).await;
    assert_eq!(structured(&r)["returned"], 1);
    assert_eq!(structured(&r)["truncated"], false);
}

#[tokio::test]
async fn read_errors_are_tool_errors() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    put(&env, "bad.csv", "\"jamais ferme");
    for (args, code) in [
        (json!({"path": "absent.csv"}), "NotFound"),
        (json!({"path": "../mod/modinfo.json"}), "PathRefused"),
        (json!({"path": "C:/Windows/win.ini"}), "PathRefused"),
        (json!({"path": "bad.csv"}), "InvalidCsv"),
        (
            json!({"path": "modinfo.json", "source": "mod"}),
            "InvalidCsv",
        ),
    ] {
        let r = s.call("read_datatable", args.clone()).await;
        assert!(is_error(&r), "{args}");
        assert!(
            text(&r).starts_with(&format!("ERREUR {code}:")),
            "{args}: {}",
            text(&r)
        );
    }
}

// --- write_datatable --------------------------------------------------------------

#[tokio::test]
async fn write_defaults_to_dry_run_and_writes_nothing() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let before_mod = tree(&env.mod_root);
    for (dry, confirm) in [(None, None), (None, Some(true)), (Some(true), Some(true))] {
        let r = write(&s, CSV_V1, dry, confirm).await;
        assert!(!is_error(&r), "{}", text(&r));
        assert!(text(&r).starts_with("DRY-RUN"), "{}", text(&r));
        assert!(text(&r).contains("Lignes ajoutées:") && text(&r).contains("Validation: OK"));
        assert_eq!(structured(&r)["dry_run"], true);
        assert_eq!(structured(&r)["applied"], false);
        assert_eq!(structured(&r)["valid"], true);
    }
    assert_eq!(fs::read_dir(&env.csv_root).unwrap().count(), 0);
    assert_eq!(tree(&env.mod_root), before_mod);
}

#[tokio::test]
async fn real_write_requires_both_dry_run_false_and_confirm_true() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    for (dry, confirm) in [(Some(false), None), (Some(false), Some(false))] {
        let r = write(&s, CSV_V1, dry, confirm).await;
        assert!(is_error(&r));
        assert!(
            text(&r).starts_with("ERREUR ConfirmRequired:"),
            "{}",
            text(&r)
        );
    }
    assert!(!env.csv_root.join("DT_Purge_Nordheimer.csv").exists());
}

#[tokio::test]
async fn create_then_overwrite_returns_the_diff_and_the_backup_path() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let r = write(&s, CSV_V1, Some(false), Some(true)).await;
    assert!(!is_error(&r), "{}", text(&r));
    assert!(text(&r).starts_with("ÉCRIT") && text(&r).contains("nouveau"));
    assert!(structured(&r)["backup"].is_null());
    let file = env.csv_root.join("DT_Purge_Nordheimer.csv");
    assert_eq!(fs::read_to_string(&file).unwrap(), CSV_V1);

    let v2 = csv_v2();
    let r = write(&s, &v2, None, None).await;
    let t = text(&r);
    assert!(t.starts_with("DRY-RUN") && t.contains("existant"), "{t}");
    assert!(
        t.contains("Résumé: 1 ajoutée(s), 1 modifiée(s), 0 supprimée(s)"),
        "{t}"
    );
    assert!(
        t.contains("~ Trash1 | Weight: \"10.000000\" -> \"12.000000\""),
        "{t}"
    );
    assert!(
        t.contains("+ Trash5 |") && t.contains("Validation: OK"),
        "{t}"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), CSV_V1);

    let r = write(&s, &v2, Some(false), Some(true)).await;
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(
        t.starts_with("ÉCRIT") && t.contains("Sauvegarde créée: .backup/DT_Purge_Nordheimer."),
        "{t}"
    );
    let backup = structured(&r)["backup_real_path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(fs::read_to_string(&backup).unwrap(), CSV_V1);
    assert_eq!(fs::read_to_string(&file).unwrap(), v2);

    let r = write(&s, &v2, Some(false), Some(true)).await;
    assert!(text(&r).starts_with("AUCUN CHANGEMENT"), "{}", text(&r));
}

#[tokio::test]
async fn write_to_a_custom_path_and_core_errors_are_tool_errors() {
    let env = Env::new();
    // CSV_ROOT = parent du mod: "mod/..." est atteignable mais doit être refusé
    let cfg = conan_core::Config::new(&env.mod_root, Some(env.tmp.path().to_path_buf()))
        .with_schema_root(&env.schema_root);
    let s = connect(&cfg).await;
    let before_mod = tree(&env.mod_root);
    let with_path = |path: &str| json!({"table": TABLE, "content": CSV_V1, "path": path, "dry_run": false, "confirm": true});
    for (path, code) in [
        ("mod/Content/new.csv", "ModReadOnly"),
        ("mod/active.txt", "ModReadOnly"),
        ("../x.csv", "PathRefused"),
        ("/x.csv", "PathRefused"),
        ("D:/x.csv", "PathRefused"),
        ("a\\b.csv", "PathRefused"),
        ("NUL.csv", "PathRefused"),
        (".backup/x.csv", "ReservedPath"),
    ] {
        let r = s.call("write_datatable", with_path(path)).await;
        assert!(is_error(&r), "{path}");
        assert!(
            text(&r).starts_with(&format!("ERREUR {code}:")),
            "{path}: {}",
            text(&r)
        );
    }
    let r = s
        .call(
            "write_datatable",
            write_args(TABLE, "\"jamais ferme", Some(false), Some(true)),
        )
        .await;
    assert!(text(&r).starts_with("ERREUR InvalidCsv:"), "{}", text(&r));
    assert_eq!(tree(&env.mod_root), before_mod, "le mod n'a pas bougé");
    // un chemin personnalisé valide fonctionne
    let r = s.call("write_datatable", with_path("sub/autre.csv")).await;
    assert!(!is_error(&r), "{}", text(&r));
    assert!(env.tmp.path().join("sub/autre.csv").is_file());
}

#[tokio::test]
async fn schemas_directory_under_csv_root_cannot_be_overwritten_through_write() {
    let env = Env::new();
    // SCHEMA_ROOT placé (par erreur) sous CSV_ROOT: toujours en lecture seule
    let schema_root = env.csv_root.join("schemas");
    fs::create_dir_all(&schema_root).unwrap();
    fs::write(schema_root.join(format!("{TABLE}.toml")), SCHEMA_TOML).unwrap();
    let cfg = conan_core::Config::new(&env.mod_root, Some(env.csv_root.clone()))
        .with_schema_root(&schema_root);
    let s = connect(&cfg).await;
    let before = tree(&schema_root);
    let r = s
        .call("write_datatable", json!({"table": TABLE, "content": CSV_V1, "path": format!("schemas/{TABLE}.toml"), "dry_run": false, "confirm": true}))
        .await;
    assert!(
        text(&r).starts_with("ERREUR SchemaReadOnly:"),
        "{}",
        text(&r)
    );
    assert_eq!(tree(&schema_root), before);
}

#[tokio::test]
async fn backup_failure_and_unreadable_existing_file_leave_the_original_intact() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    write(&s, CSV_V1, Some(false), Some(true)).await;
    fs::write(env.csv_root.join(".backup"), "obstacle").unwrap();
    let r = write(&s, &csv_v2(), Some(false), Some(true)).await;
    assert!(is_error(&r));
    assert!(text(&r).starts_with("ERREUR BackupFailed:") && text(&r).contains("n'a pas eu lieu"));
    assert_eq!(
        fs::read_to_string(env.csv_root.join("DT_Purge_Nordheimer.csv")).unwrap(),
        CSV_V1
    );

    put(&env, "bad.csv", "\"jamais ferme");
    let r = s
        .call("write_datatable", json!({"table": TABLE, "content": CSV_V1, "path": "bad.csv", "dry_run": false, "confirm": true}))
        .await;
    assert!(
        text(&r).starts_with("ERREUR ExistingUnreadable:"),
        "{}",
        text(&r)
    );
    assert_eq!(
        fs::read(env.csv_root.join("bad.csv")).unwrap(),
        b"\"jamais ferme"
    );
}

#[tokio::test]
async fn links_escaping_csv_root_are_refused_by_every_tool() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let outside = env.tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.csv"), CSV_V1).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, env.csv_root.join("link")).unwrap();
    #[cfg(windows)]
    {
        let out = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(env.csv_root.join("link"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    let r = s
        .call("read_datatable", json!({"path": "link/secret.csv"}))
        .await;
    assert!(text(&r).starts_with("ERREUR PathEscapes:"), "{}", text(&r));
    let r = s
        .call("write_datatable", json!({"table": TABLE, "content": CSV_V1, "path": "link/new.csv", "dry_run": false, "confirm": true}))
        .await;
    assert!(text(&r).starts_with("ERREUR PathEscapes:"), "{}", text(&r));
    let r = s
        .call(
            "validate_rows",
            json!({"table": TABLE, "path": "link/secret.csv"}),
        )
        .await;
    assert!(text(&r).starts_with("ERREUR PathEscapes:"), "{}", text(&r));
    let r = s.call("list_mod_files", json!({"subfolder": "link"})).await;
    assert!(text(&r).starts_with("ERREUR PathEscapes:"), "{}", text(&r));
    let r = s.call("list_mod_files", json!({})).await;
    assert!(!serde_json::to_string(structured(&r))
        .unwrap()
        .contains("secret"));
    assert!(!outside.join("new.csv").exists());
}

#[tokio::test]
async fn mod_files_are_never_modified_by_any_flow() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let before = tree(&env.mod_root);
    s.call("list_mod_files", json!({"source": "mod"})).await;
    s.call(
        "read_datatable",
        json!({"path": "Content/Data/m.csv", "source": "mod"}),
    )
    .await;
    write(&s, CSV_V1, None, None).await;
    write(&s, CSV_V1, Some(false), Some(true)).await;
    write(&s, &csv_v2(), Some(false), Some(true)).await;
    s.call("write_datatable", json!({"table": TABLE, "content": csv_v2(), "path": "../mod/Content/Data/m.csv", "dry_run": false, "confirm": true})).await;
    assert_eq!(tree(&env.mod_root), before);
}
