//! Schémas et validation obligatoire: list_schemas, describe_schema, validate_rows, et le refus
//! d'écriture sans schéma valide. Table réelle: DT_Purge_Nordheimer.

mod common;

use common::*;
use serde_json::{json, Value};
use std::fs;

fn rows(n: usize) -> Value {
    json!((1..=n)
        .map(|i| vec![
            format!("Trash{i}"),
            "Nordheimer".into(),
            "PurgeNew_X".into(),
            "1.000000".into(),
            "WeightedSpawnTable".into()
        ])
        .collect::<Vec<_>>())
}

// --- SCHEMA_ROOT absent ou invalide -----------------------------------------------

#[tokio::test]
async fn without_schema_root_reads_work_and_every_write_is_refused() {
    let env = Env::new();
    let s = connect(&env.config_without_schemas()).await;
    // lecture: inchangée
    let r = s.call("list_mod_files", json!({"source": "mod"})).await;
    assert!(!is_error(&r), "{}", text(&r));
    let r = s
        .call(
            "read_datatable",
            json!({"path": "Content/Data/m.csv", "source": "mod"}),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    // écriture: refusée, dry-run compris, avec un message explicite
    for (dry, confirm) in [(None, None), (Some(false), Some(true))] {
        let r = s
            .call("write_datatable", write_args(TABLE, CSV_V1, dry, confirm))
            .await;
        assert!(is_error(&r));
        let t = text(&r);
        assert!(t.starts_with("ERREUR SchemaRootUnavailable:"), "{t}");
        assert!(
            t.contains("SCHEMA_ROOT n'est pas configuré")
                && t.contains("toute écriture est refusée"),
            "{t}"
        );
    }
    for tool in ["list_schemas"] {
        let r = s.call(tool, json!({})).await;
        assert!(
            text(&r).starts_with("ERREUR SchemaRootUnavailable:"),
            "{}",
            text(&r)
        );
    }
    for (tool, args) in [
        ("describe_schema", json!({"table": TABLE})),
        ("validate_rows", json!({"table": TABLE, "rows": []})),
    ] {
        let r = s.call(tool, args).await;
        assert!(
            text(&r).starts_with("ERREUR SchemaRootUnavailable:"),
            "{tool}: {}",
            text(&r)
        );
    }
    assert_eq!(
        fs::read_dir(&env.csv_root).unwrap().count(),
        0,
        "rien d'écrit"
    );
}

#[tokio::test]
async fn invalid_schema_root_starts_the_server_and_refuses_writes() {
    let env = Env::new();
    for bad in [
        env.tmp.path().join("absent"),
        env.mod_root.join("modinfo.json"),
        std::path::PathBuf::new(),
    ] {
        let cfg = conan_core::Config::new(&env.mod_root, Some(env.csv_root.clone()))
            .with_schema_root(&bad);
        let s = connect(&cfg).await; // démarre malgré tout
        let r = s
            .call(
                "write_datatable",
                write_args(TABLE, CSV_V1, Some(false), Some(true)),
            )
            .await;
        assert!(
            text(&r).starts_with("ERREUR SchemaRootUnavailable:"),
            "{bad:?}: {}",
            text(&r)
        );
        assert!(text(&r).contains("SCHEMA_ROOT"), "{}", text(&r));
        let r = s.call("list_mod_files", json!({})).await;
        assert!(!is_error(&r));
    }
    assert!(!env.csv_root.join("DT_Purge_Nordheimer.csv").exists());
}

// --- schéma absent / malformé / évasion -------------------------------------------

#[tokio::test]
async fn missing_schema_means_no_write_not_even_a_dry_run() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    for (dry, confirm) in [(None, None), (Some(false), Some(true))] {
        let r = s
            .call(
                "write_datatable",
                write_args("DT_Inconnue", CSV_V1, dry, confirm),
            )
            .await;
        assert!(is_error(&r));
        let t = text(&r);
        assert!(
            t.starts_with("ERREUR SchemaMissing:")
                && t.contains("DT_Inconnue.toml")
                && t.contains("Pas de schéma, pas d'écriture"),
            "{t}"
        );
    }
    for (tool, args) in [
        ("describe_schema", json!({"table": "DT_Inconnue"})),
        ("validate_rows", json!({"table": "DT_Inconnue", "rows": []})),
    ] {
        assert!(text(&s.call(tool, args).await).starts_with("ERREUR SchemaMissing:"));
    }
    assert_eq!(fs::read_dir(&env.csv_root).unwrap().count(), 0);
}

#[tokio::test]
async fn malformed_schema_is_blocking_and_names_the_file_and_the_field() {
    let env = Env::new();
    fs::write(
        env.schema_root.join("DT_Casse.toml"),
        "name = \"DT_Casse\"\nkey_column = \"---\"\n[[fields]]\nname = \"Poids\"\ntype = \"decimal\"\n",
    )
    .unwrap();
    fs::write(
        env.schema_root.join("DT_Autre.toml"),
        "name = \"DT_Autre\"\n",
    )
    .unwrap(); // sans key_column
    fs::write(
        env.schema_root.join("DT_Nom.toml"),
        "name = \"DT_Mauvais\"\nkey_column = \"---\"\n",
    )
    .unwrap();
    let s = connect(&env.config()).await;

    for (table, needles) in [
        ("DT_Casse", vec!["DT_Casse.toml", "Poids", "decimal"]),
        ("DT_Autre", vec!["DT_Autre.toml", "key_column"]),
        ("DT_Nom", vec!["DT_Nom.toml", "DT_Mauvais"]),
    ] {
        for (tool, args) in [
            ("write_datatable", write_args(table, CSV_V1, None, None)),
            ("describe_schema", json!({"table": table})),
            ("validate_rows", json!({"table": table, "rows": []})),
        ] {
            let r = s.call(tool, args).await;
            let t = text(&r);
            assert!(
                is_error(&r) && t.starts_with("ERREUR SchemaMalformed:"),
                "{table}/{tool}: {t}"
            );
            assert!(needles.iter().all(|n| t.contains(n)), "{table}/{tool}: {t}");
        }
    }
    // la table saine reste utilisable
    let r = s
        .call("write_datatable", write_args(TABLE, CSV_V1, None, None))
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    // et list_schemas signale chaque problème sans s'arrêter
    let r = s.call("list_schemas", json!({})).await;
    let v = structured(&r);
    assert_eq!(v["count"], 4);
    let by = |t: &str| {
        v["schemas"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["table"] == t)
            .unwrap()
            .clone()
    };
    assert_eq!(by(TABLE)["valid"], true);
    assert_eq!(by("DT_Casse")["valid"], false);
    assert!(by("DT_Casse")["error"].as_str().unwrap().contains("Poids"));
    assert_eq!(by("DT_Autre")["error_code"], "SchemaMalformed");
}

#[tokio::test]
async fn table_names_cannot_escape_schema_root() {
    let env = Env::new();
    fs::write(env.tmp.path().join("secret.toml"), SCHEMA_TOML).unwrap();
    let s = connect(&env.config()).await;
    for table in [
        "../secret",
        "..",
        "a/b",
        "/etc/passwd",
        "C:/x",
        "..\\secret",
        "secret.toml",
        "NUL",
        "",
    ] {
        for (tool, args) in [
            ("describe_schema", json!({"table": table})),
            ("write_datatable", write_args(table, CSV_V1, None, None)),
        ] {
            let r = s.call(tool, args).await;
            let t = text(&r);
            assert!(
                is_error(&r)
                    && (t.starts_with("ERREUR InvalidTableName:")
                        || t.starts_with("ERREUR PathRefused:")),
                "{table:?}/{tool}: {t}"
            );
        }
    }
    assert_eq!(fs::read_dir(&env.csv_root).unwrap().count(), 0);
}

#[tokio::test]
async fn schema_files_linking_outside_schema_root_are_refused() {
    let env = Env::new();
    let outside = env.tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        outside.join("DT_Lien.toml"),
        SCHEMA_TOML.replace(TABLE, "DT_Lien"),
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.join("DT_Lien.toml"),
        env.schema_root.join("DT_Lien.toml"),
    )
    .unwrap();
    #[cfg(windows)]
    {
        // fichier lié impossible sans privilège; une jonction de dossier ne donne pas accès via un nom de table
        let out = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(env.schema_root.join("sub"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    let s = connect(&env.config()).await;
    #[cfg(unix)]
    {
        let r = s.call("describe_schema", json!({"table": "DT_Lien"})).await;
        assert!(text(&r).starts_with("ERREUR PathEscapes:"), "{}", text(&r));
        let r = s
            .call(
                "write_datatable",
                write_args("DT_Lien", CSV_V1, Some(false), Some(true)),
            )
            .await;
        assert!(text(&r).starts_with("ERREUR PathEscapes:"), "{}", text(&r));
    }
    let r = s
        .call("describe_schema", json!({"table": "sub/DT_Lien"}))
        .await;
    assert!(is_error(&r));
    let r = s.call("list_schemas", json!({})).await;
    let listed = serde_json::to_string(structured(&r)).unwrap();
    assert!(!listed.contains("DT_Lien"), "{listed}");
    assert_eq!(fs::read_dir(&env.csv_root).unwrap().count(), 0);
}

// --- list_schemas / describe_schema -----------------------------------------------

#[tokio::test]
async fn list_and_describe_the_real_schema() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let r = s.call("list_schemas", json!({})).await;
    assert!(!is_error(&r), "{}", text(&r));
    let v = structured(&r);
    assert_eq!(v["count"], 1);
    assert_eq!(v["schemas"][0]["table"], TABLE);
    assert_eq!(v["schemas"][0]["valid"], true);
    assert_eq!(v["schemas"][0]["columns"], 4);

    let r = s.call("describe_schema", json!({"table": TABLE})).await;
    assert!(!is_error(&r), "{}", text(&r));
    let v = structured(&r);
    assert_eq!(v["csv_header"], "---,SourceName,TargetName,Weight,RowType");
    assert_eq!(v["key_column"], "---");
    assert_eq!(v["default_path"], "DT_Purge_Nordheimer.csv");
    let cols = v["columns"].as_array().unwrap();
    let names: Vec<_> = cols.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["SourceName", "TargetName", "Weight", "RowType"]);
    assert_eq!(cols[2]["type"], "float");
    assert_eq!(cols[3]["type"], "enum");
    assert_eq!(cols[3]["values"], json!(["WeightedSpawnTable"]));
    assert!(cols.iter().all(|c| c["required"] == true));
}

// --- validate_rows ----------------------------------------------------------------

#[tokio::test]
async fn validate_rows_ok_and_structured_error_report() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let r = s
        .call("validate_rows", json!({"table": TABLE, "rows": rows(3)}))
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert!(text(&r).starts_with("VALIDATION OK"), "{}", text(&r));
    assert_eq!(structured(&r)["valid"], true);

    let bad = json!([
        ["Trash1", "N", "T", "abc", "WeightedSpawnTable"],
        ["Trash2", "", "T", "1.0", "Inconnu"],
        ["Trash1", "N", "T", "1.0", "WeightedSpawnTable"],
        ["Trash9", "N"],
    ]);
    let r = s
        .call("validate_rows", json!({"table": TABLE, "rows": bad}))
        .await;
    assert!(
        !is_error(&r),
        "un diagnostic n'est pas une erreur d'outil: {}",
        text(&r)
    );
    assert!(text(&r).starts_with("VALIDATION ÉCHOUÉE"), "{}", text(&r));
    let v = structured(&r);
    assert_eq!(v["valid"], false);
    assert_eq!(v["schema_drift"], false);
    // 1 (nombre de valeurs) seulement: une ligne mal formée empêche de lire le tableau
    assert_eq!(v["errors"][0]["rule"], "ColumnCount");
    assert_eq!(v["errors"][0]["line"], 5);
    assert_eq!(v["errors"][0]["key"], "Trash9");

    // sans la ligne mal formée: toutes les erreurs de valeur sont listées
    let bad = json!([
        ["Trash1", "N", "T", "abc", "WeightedSpawnTable"],
        ["Trash2", "", "T", "1.0", "Inconnu"],
        ["trash1", "N", "T", "1.0", "WeightedSpawnTable"],
    ]);
    let r = s
        .call("validate_rows", json!({"table": TABLE, "rows": bad}))
        .await;
    let v = structured(&r);
    let got: Vec<_> = v["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["line"].as_u64().unwrap(),
                e["key"].as_str().unwrap().to_owned(),
                e["column"].as_str().map(str::to_owned),
                e["value"].as_str().unwrap().to_owned(),
                e["rule"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                2,
                "Trash1".into(),
                Some("Weight".into()),
                "abc".into(),
                "WrongType".into()
            ),
            (
                3,
                "Trash2".into(),
                Some("SourceName".into()),
                "".into(),
                "RequiredEmpty".into()
            ),
            (
                3,
                "Trash2".into(),
                Some("RowType".into()),
                "Inconnu".into(),
                "NotInEnum".into()
            ),
            (
                4,
                "trash1".into(),
                None,
                "trash1".into(),
                "DuplicateKey".into()
            ),
        ]
    );
    assert_eq!(v["error_count"], 4);
    assert!(text(&r).contains("[WrongType]") && text(&r).contains("(clé Trash2)"));
}

#[tokio::test]
async fn validate_existing_csv_is_a_corruption_diagnostic_without_repair() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    // absent
    let r = s.call("validate_rows", json!({"table": TABLE})).await;
    assert!(text(&r).starts_with("ERREUR NotFound:"), "{}", text(&r));
    // valide
    s.call(
        "write_datatable",
        write_args(TABLE, CSV_V1, Some(false), Some(true)),
    )
    .await;
    let r = s.call("validate_rows", json!({"table": TABLE})).await;
    assert!(text(&r).starts_with("VALIDATION OK"), "{}", text(&r));
    assert_eq!(structured(&r)["source"], "csv");
    // corrompu à la main: valeur invalide + dérive d'en-tête sur un autre fichier
    let file = env.csv_root.join("DT_Purge_Nordheimer.csv");
    let corrupted = CSV_V1.replace("\"5.000000\"", "\"cinq\"");
    fs::write(&file, &corrupted).unwrap();
    let r = s.call("validate_rows", json!({"table": TABLE})).await;
    assert!(!is_error(&r));
    assert_eq!(structured(&r)["errors"][0]["rule"], "WrongType");
    assert_eq!(structured(&r)["errors"][0]["key"], "Trash3");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        corrupted,
        "aucune réparation"
    );

    fs::write(
        env.csv_root.join("derive.csv"),
        CSV_V1.replacen("RowType", "RowKind", 1),
    )
    .unwrap();
    let r = s
        .call(
            "validate_rows",
            json!({"table": TABLE, "path": "derive.csv"}),
        )
        .await;
    assert_eq!(structured(&r)["schema_drift"], true);
    assert!(text(&r).contains("DÉRIVE DE SCHÉMA"), "{}", text(&r));

    // fichier illisible: erreur d'outil (la corruption est syntaxique)
    fs::write(env.csv_root.join("casse.csv"), "\"jamais ferme").unwrap();
    let r = s
        .call(
            "validate_rows",
            json!({"table": TABLE, "path": "casse.csv"}),
        )
        .await;
    assert!(
        text(&r).starts_with("ERREUR InvalidCsv:") && text(&r).contains("corruption"),
        "{}",
        text(&r)
    );
}

// --- write_datatable : validation obligatoire --------------------------------------

#[tokio::test]
async fn header_drift_blocks_the_real_write_and_is_reported_in_dry_run() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let drifts = [
        CSV_V1.replacen("Weight,RowType", "RowType,Weight", 1), // ordre
        CSV_V1.replacen("Weight", "Poids", 1),                  // renommée
        CSV_V1.replacen("---", "Name", 1),                      // clé
        "---,SourceName,TargetName,Weight\nTrash1,\"N\",\"T\",\"1.0\"\n".to_owned(), // colonne en moins
    ];
    for csv in drifts {
        let r = write(&s, &csv, None, None).await;
        assert!(!is_error(&r), "dry-run: {}", text(&r));
        assert!(
            text(&r).starts_with("DRY-RUN — VALIDATION ÉCHOUÉE")
                && text(&r).contains("DÉRIVE DE SCHÉMA"),
            "{}",
            text(&r)
        );
        assert_eq!(structured(&r)["schema_drift"], true);
        assert_eq!(structured(&r)["valid"], false);

        let r = write(&s, &csv, Some(false), Some(true)).await;
        assert!(is_error(&r));
        assert!(
            text(&r).starts_with("ERREUR SchemaDrift:") && text(&r).contains("Rien n'a été écrit"),
            "{}",
            text(&r)
        );
        assert_eq!(structured(&r)["schema_drift"], true);
    }
    assert_eq!(fs::read_dir(&env.csv_root).unwrap().count(), 0);
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
async fn recette_valid_dry_run_then_invalid_dry_run_then_real_write() {
    let env = Env::new();
    let s = connect(&env.config()).await;
    let file = env.csv_root.join("DT_Purge_Nordheimer.csv");

    // 1. dry-run valide
    let r = write(&s, CSV_V1, None, None).await;
    assert!(
        text(&r).starts_with("DRY-RUN (aperçu") && text(&r).contains("Validation: OK"),
        "{}",
        text(&r)
    );
    assert!(!file.exists());

    // 2. dry-run invalide, avec rapport d'erreurs
    let invalid = CSV_V1
        .replace("\"WeightedSpawnTable\"\nTrash2", "\"Bogus\"\nTrash2")
        .replace("\"3.000000\"", "\"trois\"");
    let r = write(&s, &invalid, None, None).await;
    let t = text(&r);
    assert!(t.starts_with("DRY-RUN — VALIDATION ÉCHOUÉE"), "{t}");
    assert!(
        t.contains("Validation: 3 erreur(s)") || t.contains("Validation: 2 erreur(s)"),
        "{t}"
    );
    assert!(
        t.contains("[NotInEnum]") && t.contains("[WrongType]") && t.contains("Lignes ajoutées:"),
        "{t}"
    );
    assert!(!file.exists());

    // 3. écriture réelle de l'invalide: refusée, aucun mode forcer
    let r = write(&s, &invalid, Some(false), Some(true)).await;
    assert!(
        is_error(&r) && text(&r).starts_with("ERREUR ValidationFailed:"),
        "{}",
        text(&r)
    );
    assert!(!file.exists());

    // 4. écriture réelle du valide
    let r = write(&s, CSV_V1, Some(false), Some(true)).await;
    assert!(
        !is_error(&r) && text(&r).starts_with("ÉCRIT"),
        "{}",
        text(&r)
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), CSV_V1);
    let r = s.call("validate_rows", json!({"table": TABLE})).await;
    assert!(text(&r).starts_with("VALIDATION OK"));
}
