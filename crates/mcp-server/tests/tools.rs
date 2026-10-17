//! Tests d'intégration client↔serveur en mémoire: chaque outil, cas nominal et cas d'erreur.

mod common;

use common::*;
use serde_json::json;
use std::fs;

#[tokio::test]
async fn exposes_exactly_the_three_tools_with_descriptions() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let tools = s.client.list_all_tools().await.unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        ["list_mod_files", "read_datatable", "write_datatable"]
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
    assert!(schema.contains("dry_run") && schema.contains("confirm") && schema.contains("content"));
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
    assert!(!is_error(&r), "{}", text(&r));
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
    for (content, confirm) in [(CSV_V1, true), (CSV_V2, true)] {
        let r = s
            .call(
                "write_datatable",
                write_args("d/a.csv", content, Some(false), Some(confirm)),
            )
            .await;
        assert!(!is_error(&r), "{}", text(&r));
    }
    // la 2e écriture a créé une sauvegarde; on ajoute un temporaire orphelin
    assert!(env.csv_root.join(".backup").is_dir());
    fs::write(env.csv_root.join("d/.a.csv.tmp-1-2"), "x").unwrap();
    let r = s.call("list_mod_files", json!({})).await;
    let all = serde_json::to_string(structured(&r)).unwrap();
    assert!(!all.contains(".backup") && !all.contains(".tmp-"), "{all}");
    assert_eq!(structured(&r)["count"], 1);
    // lister explicitement .backup est refusé
    let r = s
        .call("list_mod_files", json!({"subfolder": ".backup"}))
        .await;
    assert!(is_error(&r));
    assert!(
        text(&r).starts_with("ERREUR InternalFolder"),
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
    s.call(
        "write_datatable",
        write_args("a.csv", CSV_V2, Some(false), Some(true)),
    )
    .await;
    let r = s.call("read_datatable", json!({"path": "a.csv"})).await;
    assert!(!is_error(&r), "{}", text(&r));
    let v = structured(&r);
    assert_eq!(v["columns"], json!(["---", "A", "B"]));
    assert_eq!(v["row_count"], 3);
    assert_eq!(v["rows"][1], json!(["2", "Y", "20"]));
    assert_eq!(
        v["format"],
        json!({"bom": false, "newline": "LF", "final_newline": true})
    );
    assert_eq!(v["truncated"], false);
}

#[tokio::test]
async fn read_paginates_and_reads_the_mod_read_only() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let r = s
        .call(
            "read_datatable",
            json!({"path": "Content/Data/m.csv", "source": "mod", "limit": 1, "offset": 1}),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    let v = structured(&r);
    assert_eq!(v["rows"], json!([["2", "y", "20"]]));
    assert_eq!(v["returned"], 1);
    assert_eq!(v["truncated"], false);
    let r = s
        .call(
            "read_datatable",
            json!({"path": "Content/Data/m.csv", "source": "mod", "limit": 1}),
        )
        .await;
    assert_eq!(structured(&r)["truncated"], true);
}

#[tokio::test]
async fn read_errors_are_tool_errors() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    fs::create_dir_all(&env.csv_root).unwrap();
    fs::write(env.csv_root.join("bad.csv"), "\"jamais ferme").unwrap();
    for (args, code) in [
        (json!({"path": "absent.csv"}), "NotFound"),
        (json!({"path": "../mod/modinfo.json"}), "PathRefused"),
        (json!({"path": "C:/Windows/win.ini"}), "PathRefused"),
        (json!({"path": "bad.csv"}), "InvalidCsv"),
        (
            json!({"path": "modinfo.json", "source": "mod"}),
            "InvalidCsv",
        ), // JSON, pas un CSV
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
    // ni dry_run ni confirm; puis confirm seul: toujours un aperçu
    for args in [
        write_args("n/a.csv", CSV_V1, None, None),
        write_args("n/a.csv", CSV_V1, None, Some(true)),
        write_args("n/a.csv", CSV_V1, Some(true), Some(true)),
    ] {
        let r = s.call("write_datatable", args).await;
        assert!(!is_error(&r), "{}", text(&r));
        assert!(text(&r).starts_with("DRY-RUN"), "{}", text(&r));
        assert!(text(&r).contains("Lignes ajoutées:") && text(&r).contains("+ 1 | 1, x, 10"));
        assert_eq!(structured(&r)["dry_run"], true);
        assert_eq!(structured(&r)["applied"], false);
    }
    assert!(
        !env.csv_root.join("n").exists(),
        "aucun dossier ni fichier créé"
    );
    assert_eq!(fs::read_dir(&env.csv_root).unwrap().count(), 0);
    assert_eq!(tree(&env.mod_root), before_mod);
}

#[tokio::test]
async fn real_write_requires_both_dry_run_false_and_confirm_true() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    for args in [
        write_args("a.csv", CSV_V1, Some(false), None),
        write_args("a.csv", CSV_V1, Some(false), Some(false)),
    ] {
        let r = s.call("write_datatable", args).await;
        assert!(is_error(&r));
        assert!(
            text(&r).starts_with("ERREUR ConfirmRequired:"),
            "{}",
            text(&r)
        );
    }
    assert!(!env.csv_root.join("a.csv").exists());
}

#[tokio::test]
async fn create_then_overwrite_returns_the_diff_and_the_backup_path() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let r = s
        .call(
            "write_datatable",
            write_args("d/a.csv", CSV_V1, Some(false), Some(true)),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert!(text(&r).starts_with("ÉCRIT") && text(&r).contains("nouveau"));
    assert!(structured(&r)["backup"].is_null());
    assert_eq!(
        fs::read_to_string(env.csv_root.join("d/a.csv")).unwrap(),
        CSV_V1
    );

    // aperçu d'abord
    let r = s
        .call("write_datatable", write_args("d/a.csv", CSV_V2, None, None))
        .await;
    let t = text(&r);
    assert!(t.starts_with("DRY-RUN") && t.contains("existant"), "{t}");
    assert!(
        t.contains("Résumé: 1 ajoutée(s), 1 modifiée(s), 0 supprimée(s)"),
        "{t}"
    );
    assert!(
        t.contains("~ 2 | A: \"y\" -> \"Y\"") && t.contains("+ 3 | 3, z, 30"),
        "{t}"
    );
    assert_eq!(
        fs::read_to_string(env.csv_root.join("d/a.csv")).unwrap(),
        CSV_V1
    );

    // puis écriture réelle
    let r = s
        .call(
            "write_datatable",
            write_args("d/a.csv", CSV_V2, Some(false), Some(true)),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(
        t.starts_with("ÉCRIT") && t.contains("Sauvegarde créée: .backup/d/a."),
        "{t}"
    );
    let backup = structured(&r)["backup_real_path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(backup.contains(".backup"));
    assert_eq!(
        fs::read_to_string(&backup).unwrap(),
        CSV_V1,
        "sauvegarde = original"
    );
    assert_eq!(
        fs::read_to_string(env.csv_root.join("d/a.csv")).unwrap(),
        CSV_V2
    );

    // contenu identique: rien à faire
    let r = s
        .call(
            "write_datatable",
            write_args("d/a.csv", CSV_V2, Some(false), Some(true)),
        )
        .await;
    assert!(text(&r).starts_with("AUCUN CHANGEMENT"), "{}", text(&r));
    assert!(structured(&r)["backup"].is_null());
}

#[tokio::test]
async fn write_errors_are_tool_errors_with_readable_messages() {
    let env = Env::new();
    // CSV_ROOT = parent du mod: "mod/..." est donc atteignable mais doit être refusé
    let cfg = conan_core::Config::new(&env.mod_root, Some(env.tmp.path().to_path_buf()));
    let s = connect(&cfg).await;
    let before_mod = tree(&env.mod_root);
    let cases = [
        ("mod/Content/new.csv", CSV_V1, "ModReadOnly"),
        ("mod/active.txt", CSV_V1, "ModReadOnly"),
        ("../x.csv", CSV_V1, "PathRefused"),
        ("/x.csv", CSV_V1, "PathRefused"),
        ("D:/x.csv", CSV_V1, "PathRefused"),
        ("a\\b.csv", CSV_V1, "PathRefused"),
        ("NUL.csv", CSV_V1, "PathRefused"),
        (".backup/x.csv", CSV_V1, "ReservedPath"),
        ("ok/x.csv", "\"jamais ferme", "InvalidCsv"),
        ("ok/x.csv", "---,A\n1\n", "InvalidCsv"),
    ];
    for (path, content, code) in cases {
        let r = s
            .call(
                "write_datatable",
                write_args(path, content, Some(false), Some(true)),
            )
            .await;
        assert!(is_error(&r), "{path}");
        assert!(
            text(&r).starts_with(&format!("ERREUR {code}:")),
            "{path}: {}",
            text(&r)
        );
    }
    // même refus en dry_run
    let r = s
        .call(
            "write_datatable",
            write_args("mod/new.csv", CSV_V1, None, None),
        )
        .await;
    assert!(text(&r).starts_with("ERREUR ModReadOnly:") && text(&r).contains("lecture seule"));
    assert_eq!(tree(&env.mod_root), before_mod, "le mod n'a pas bougé");
}

#[tokio::test]
async fn backup_failure_and_unreadable_existing_file_leave_the_original_intact() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    s.call(
        "write_datatable",
        write_args("a.csv", CSV_V1, Some(false), Some(true)),
    )
    .await;
    // `.backup` est un fichier: la sauvegarde est impossible
    fs::write(env.csv_root.join(".backup"), "obstacle").unwrap();
    let r = s
        .call(
            "write_datatable",
            write_args("a.csv", CSV_V2, Some(false), Some(true)),
        )
        .await;
    assert!(is_error(&r));
    assert!(text(&r).starts_with("ERREUR BackupFailed:") && text(&r).contains("n'a pas eu lieu"));
    assert_eq!(
        fs::read_to_string(env.csv_root.join("a.csv")).unwrap(),
        CSV_V1
    );

    fs::write(env.csv_root.join("bad.csv"), "\"jamais ferme").unwrap();
    let r = s
        .call(
            "write_datatable",
            write_args("bad.csv", CSV_V1, Some(false), Some(true)),
        )
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
        .call(
            "write_datatable",
            write_args("link/new.csv", CSV_V1, Some(false), Some(true)),
        )
        .await;
    assert!(text(&r).starts_with("ERREUR PathEscapes:"), "{}", text(&r));
    let r = s.call("list_mod_files", json!({"subfolder": "link"})).await;
    assert!(text(&r).starts_with("ERREUR PathEscapes:"), "{}", text(&r));
    // jamais listé non plus dans le listage global
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
    s.call("write_datatable", write_args("a.csv", CSV_V1, None, None))
        .await;
    s.call(
        "write_datatable",
        write_args("a.csv", CSV_V1, Some(false), Some(true)),
    )
    .await;
    s.call(
        "write_datatable",
        write_args("a.csv", CSV_V2, Some(false), Some(true)),
    )
    .await;
    // tentative de "réécrire" un fichier du mod par son nom
    s.call(
        "write_datatable",
        write_args("../mod/Content/Data/m.csv", CSV_V2, Some(false), Some(true)),
    )
    .await;
    assert_eq!(tree(&env.mod_root), before);
}
