//! Outils par ligne add_row / update_row / delete_row, client↔serveur en mémoire, sur des copies
//! temporaires du fixture réel (jamais sur D:\).

mod common;

use common::*;
use serde_json::{json, Value};
use std::fs;

const FILE: &str = "DT_Purge_Nordheimer.csv";

/// Serveur dont CSV_ROOT contient déjà une copie du fixture.
async fn setup() -> (Env, Session) {
    let env = Env::new();
    fs::create_dir_all(&env.csv_root).unwrap();
    fs::write(env.csv_root.join(FILE), CSV_V1).unwrap();
    let s = connect(&env.config()).await;
    (env, s)
}

fn file(env: &Env) -> String {
    fs::read_to_string(env.csv_root.join(FILE)).unwrap()
}

fn with(mut base: Value, extra: Value) -> Value {
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

fn new_row() -> Value {
    json!({"SourceName": "Nordheimer", "TargetName": "PurgeNew_Nordheimer_Grunt_3", "Weight": 2, "RowType": "WeightedSpawnTable"})
}

fn backups(env: &Env) -> usize {
    fs::read_dir(env.csv_root.join(".backup"))
        .map(|d| d.count())
        .unwrap_or(0)
}

// --- add_row ----------------------------------------------------------------------

#[tokio::test]
async fn add_row_dry_run_by_default_then_real_with_the_exact_devkit_bytes() {
    let (env, s) = setup().await;
    let args = json!({"table": TABLE, "key": "Trash5", "values": new_row()});

    let r = s.call("add_row", args.clone()).await; // ni dry_run ni confirm: aperçu
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(
        t.starts_with("DRY-RUN") && t.contains("Opération: add_row — clé Trash5"),
        "{t}"
    );
    assert!(
        t.contains("Résumé: 1 ajoutée(s), 0 modifiée(s), 0 supprimée(s)")
            && t.contains("+ Trash5 |"),
        "{t}"
    );
    assert!(t.contains("Validation: OK"));
    assert_eq!(file(&env), CSV_V1);
    assert_eq!(backups(&env), 0);
    let dry_summary = structured(&r)["diff"].clone();

    let r = s
        .call(
            "add_row",
            with(args, json!({"dry_run": false, "confirm": true})),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert!(
        text(&r).starts_with("ÉCRIT") && text(&r).contains("Sauvegarde créée: .backup/"),
        "{}",
        text(&r)
    );
    assert_eq!(
        structured(&r)["diff"],
        dry_summary,
        "même diff en dry-run et en réel"
    );
    let expected = format!("{CSV_V1}Trash5,\"Nordheimer\",\"PurgeNew_Nordheimer_Grunt_3\",\"2.000000\",\"WeightedSpawnTable\"\n");
    assert_eq!(file(&env).as_bytes(), expected.as_bytes());
    let backup = structured(&r)["backup_real_path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(fs::read_to_string(backup).unwrap(), CSV_V1);
}

#[tokio::test]
async fn add_row_refusals_are_tool_errors_with_codes() {
    let (env, s) = setup().await;
    let real = |key: &str, values: Value| json!({"table": TABLE, "key": key, "values": values, "dry_run": false, "confirm": true});
    for (args, code) in [
        (real("Trash1", new_row()), "KeyExists"),
        (real("trash1", new_row()), "KeyExists"), // variante de casse
        (real("TRASHGENERIC", new_row()), "KeyExists"),
        (real("Trash5", json!({"SourceName": "N"})), "BadColumns"), // colonnes manquantes
        (
            real("Trash5", with(new_row(), json!({"Poids": 1}))),
            "BadColumns",
        ), // colonne inconnue
        (
            real("Trash5", with(new_row(), json!({"---": "x"}))),
            "KeyNotEditable",
        ),
        (
            real("Trash5", with(new_row(), json!({"Weight": "lourd"}))),
            "ValidationFailed",
        ),
        (
            real("Trash5", with(new_row(), json!({"RowType": "Inconnu"}))),
            "ValidationFailed",
        ),
        (
            real("Trash5", with(new_row(), json!({"Weight": null}))),
            "InvalidParameter",
        ),
        (
            real("Trash5", with(new_row(), json!({"Weight": [1]}))),
            "InvalidParameter",
        ),
        (
            json!({"table": TABLE, "key": "Trash5", "values": new_row(), "dry_run": false}),
            "ConfirmRequired",
        ),
        (
            json!({"table": "DT_Inconnue", "key": "Trash5", "values": new_row()}),
            "SchemaMissing",
        ),
    ] {
        let r = s.call("add_row", args.clone()).await;
        assert!(is_error(&r), "{args}");
        assert!(
            text(&r).starts_with(&format!("ERREUR {code}:")),
            "{args}: {}",
            text(&r)
        );
    }
    assert_eq!(file(&env), CSV_V1);
    assert_eq!(backups(&env), 0);
}

#[tokio::test]
async fn add_row_dry_run_with_an_invalid_value_is_a_normal_result_with_a_report() {
    let (env, s) = setup().await;
    let r = s
        .call("add_row", json!({"table": TABLE, "key": "Trash5", "values": with(new_row(), json!({"Weight": "lourd"}))}))
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert!(
        text(&r).starts_with("DRY-RUN — VALIDATION ÉCHOUÉE") && text(&r).contains("[WrongType]"),
        "{}",
        text(&r)
    );
    assert_eq!(structured(&r)["valid"], false);
    assert_eq!(file(&env), CSV_V1);
}

// --- update_row -------------------------------------------------------------------

#[tokio::test]
async fn update_row_merges_partial_values_and_keeps_everything_else() {
    let (env, s) = setup().await;
    let args = json!({"table": TABLE, "key": "trash1", "values": {"Weight": 25}});
    let r = s.call("update_row", args.clone()).await;
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(
        t.starts_with("DRY-RUN") && t.contains("~ Trash1 | Weight: \"10.000000\" -> \"25.000000\""),
        "{t}"
    );
    assert!(
        t.contains("Résumé: 0 ajoutée(s), 1 modifiée(s), 0 supprimée(s)"),
        "{t}"
    );
    assert_eq!(file(&env), CSV_V1);

    let r = s
        .call(
            "update_row",
            with(args, json!({"dry_run": false, "confirm": true})),
        )
        .await;
    assert!(text(&r).starts_with("ÉCRIT"), "{}", text(&r));
    assert_eq!(
        file(&env).as_bytes(),
        CSV_V1.replace("\"10.000000\"", "\"25.000000\"").as_bytes()
    );
    assert_eq!(backups(&env), 1);
}

#[tokio::test]
async fn update_row_without_any_value_change_says_so_and_writes_nothing() {
    let (env, s) = setup().await;
    for values in [
        json!({"Weight": 10}),
        json!({"Weight": "10.000000"}),
        json!({"SourceName": "Nordheimer"}),
        json!({}),
    ] {
        for (dry, confirm) in [(None, None), (Some(false), Some(true))] {
            let mut args = json!({"table": TABLE, "key": "Trash1", "values": values});
            if let Some(d) = dry {
                args["dry_run"] = json!(d);
                args["confirm"] = json!(confirm.unwrap());
            }
            let r = s.call("update_row", args).await;
            assert!(!is_error(&r), "{}", text(&r));
            assert!(
                text(&r).starts_with("AUCUN CHANGEMENT"),
                "{values}: {}",
                text(&r)
            );
            assert_eq!(structured(&r)["no_change"], true);
        }
    }
    assert_eq!(file(&env), CSV_V1);
    assert_eq!(backups(&env), 0);
}

#[tokio::test]
async fn update_row_refusals() {
    let (env, s) = setup().await;
    let real = |key: &str, values: Value| json!({"table": TABLE, "key": key, "values": values, "dry_run": false, "confirm": true});
    for (args, code) in [
        (real("Trash99", json!({"Weight": 1})), "KeyNotFound"),
        (real("Trash1", json!({"---": "Autre"})), "KeyNotEditable"),
        (real("Trash1", json!({"Poids": 1})), "BadColumns"),
        (
            real("Trash1", json!({"Weight": "lourd"})),
            "ValidationFailed",
        ),
        (
            real("Trash1", json!({"RowType": "Inconnu"})),
            "ValidationFailed",
        ),
        (
            real("Trash1", json!({"SourceName": ""})),
            "ValidationFailed",
        ),
    ] {
        let r = s.call("update_row", args.clone()).await;
        assert!(is_error(&r), "{args}");
        assert!(
            text(&r).starts_with(&format!("ERREUR {code}:")),
            "{args}: {}",
            text(&r)
        );
    }
    assert_eq!(file(&env), CSV_V1);
    assert_eq!(backups(&env), 0);
    // la clé absente liste des clés existantes pour aider
    let r = s
        .call(
            "update_row",
            json!({"table": TABLE, "key": "Trash99", "values": {"Weight": 1}}),
        )
        .await;
    assert!(text(&r).contains("Trash1, Trash2"), "{}", text(&r));
}

// --- delete_row -------------------------------------------------------------------

#[tokio::test]
async fn delete_row_shows_the_row_then_removes_it_for_real() {
    let (env, s) = setup().await;
    let r = s
        .call("delete_row", json!({"table": TABLE, "key": "TRASH3"}))
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(t.starts_with("DRY-RUN") && t.contains("Lignes supprimées:") && t.contains("- Trash3 | Trash3, NordheimerWolf, PurgeNew_Nordheimer_Wolf, 5.000000, WeightedSpawnTable"), "{t}");
    assert_eq!(file(&env), CSV_V1);

    let r = s
        .call(
            "delete_row",
            json!({"table": TABLE, "key": "TRASH3", "dry_run": false, "confirm": true}),
        )
        .await;
    assert!(text(&r).starts_with("ÉCRIT"), "{}", text(&r));
    let expected = CSV_V1.replace("Trash3,\"NordheimerWolf\",\"PurgeNew_Nordheimer_Wolf\",\"5.000000\",\"WeightedSpawnTable\"\n", "");
    assert_eq!(file(&env).as_bytes(), expected.as_bytes());
    let backup = structured(&r)["backup_real_path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(fs::read_to_string(backup).unwrap(), CSV_V1);
}

#[tokio::test]
async fn delete_row_refusals() {
    let (env, s) = setup().await;
    let r = s
        .call(
            "delete_row",
            json!({"table": TABLE, "key": "Trash99", "dry_run": false, "confirm": true}),
        )
        .await;
    assert!(text(&r).starts_with("ERREUR KeyNotFound:"), "{}", text(&r));
    let r = s
        .call(
            "delete_row",
            json!({"table": TABLE, "key": "Trash1", "dry_run": false}),
        )
        .await;
    assert!(
        text(&r).starts_with("ERREUR ConfirmRequired:"),
        "{}",
        text(&r)
    );
    assert_eq!(file(&env), CSV_V1);
    assert_eq!(backups(&env), 0);
}

// --- règles communes --------------------------------------------------------------

#[tokio::test]
async fn an_invalid_source_csv_blocks_the_three_tools_and_points_to_validate_rows() {
    let (env, s) = setup().await;
    let broken = CSV_V1.replace("\"5.000000\"", "\"cinq\"");
    fs::write(env.csv_root.join(FILE), &broken).unwrap();
    for (tool, args) in [
        (
            "add_row",
            json!({"table": TABLE, "key": "Trash5", "values": new_row()}),
        ),
        (
            "update_row",
            json!({"table": TABLE, "key": "Trash1", "values": {"Weight": 1}}),
        ),
        ("delete_row", json!({"table": TABLE, "key": "Trash1"})),
    ] {
        for real in [false, true] {
            let mut a = args.clone();
            if real {
                a["dry_run"] = json!(false);
                a["confirm"] = json!(true);
            }
            let r = s.call(tool, a).await;
            assert!(is_error(&r), "{tool}");
            let t = text(&r);
            assert!(
                t.starts_with("ERREUR SourceInvalid:")
                    && t.contains("validate_rows")
                    && t.contains("aucune réparation"),
                "{tool}: {t}"
            );
        }
    }
    assert_eq!(
        fs::read_to_string(env.csv_root.join(FILE)).unwrap(),
        broken,
        "aucune réparation"
    );
    assert_eq!(backups(&env), 0);
    // le diagnostic est bien disponible
    let r = s.call("validate_rows", json!({"table": TABLE})).await;
    assert_eq!(structured(&r)["valid"], false);
}

#[tokio::test]
async fn missing_csv_and_missing_schema_root_refuse_the_row_tools() {
    let env = Env::new();
    let s = connect(&env.config()).await; // pas de CSV
    let r = s
        .call(
            "add_row",
            json!({"table": TABLE, "key": "Trash5", "values": new_row()}),
        )
        .await;
    assert!(
        text(&r).starts_with("ERREUR NotFound:") && text(&r).contains("write_datatable"),
        "{}",
        text(&r)
    );

    let s = connect(&env.config_without_schemas()).await;
    for tool in ["add_row", "update_row", "delete_row"] {
        let r = s
            .call(
                tool,
                json!({"table": TABLE, "key": "Trash1", "values": new_row()}),
            )
            .await;
        assert!(
            text(&r).starts_with("ERREUR SchemaRootUnavailable:"),
            "{tool}: {}",
            text(&r)
        );
    }
}

#[tokio::test]
async fn the_row_tools_never_touch_the_mod() {
    let (env, s) = setup().await;
    let before = tree(&env.mod_root);
    let real = json!({"dry_run": false, "confirm": true});
    s.call(
        "add_row",
        with(
            json!({"table": TABLE, "key": "Trash5", "values": new_row()}),
            real.clone(),
        ),
    )
    .await;
    s.call(
        "update_row",
        with(
            json!({"table": TABLE, "key": "Trash5", "values": {"Weight": 3}}),
            real.clone(),
        ),
    )
    .await;
    s.call(
        "delete_row",
        with(json!({"table": TABLE, "key": "Trash5"}), real.clone()),
    )
    .await;
    // chemins hors CSV_ROOT
    for tool_args in [
        (
            "add_row",
            json!({"table": TABLE, "key": "K", "values": new_row(), "path": "../mod/Content/Data/m.csv"}),
        ),
        (
            "delete_row",
            json!({"table": TABLE, "key": "Trash1", "path": "../mod/Content/Data/m.csv"}),
        ),
    ] {
        let r = s.call(tool_args.0, with(tool_args.1, real.clone())).await;
        assert!(
            is_error(&r) && text(&r).starts_with("ERREUR PathRefused:"),
            "{}",
            text(&r)
        );
    }
    assert_eq!(tree(&env.mod_root), before);
    assert_eq!(
        file(&env),
        CSV_V1,
        "ajout, modification puis suppression de la même ligne: retour à l'original"
    );
}

#[tokio::test]
async fn tool_descriptions_are_explicit_for_the_client() {
    let (_env, s) = setup().await;
    let tools = s.client.list_all_tools().await.unwrap();
    for (name, needles) in [
        (
            "add_row",
            vec![
                "FIN",
                "TOUTES les colonnes",
                "dry_run=true",
                "confirm=true",
                "validate_rows",
            ],
        ),
        (
            "update_row",
            vec![
                "fusionnées",
                "colonne clé",
                "AUCUN CHANGEMENT",
                "dry_run=true",
            ],
        ),
        (
            "delete_row",
            vec![
                "SUPPRIME DÉFINITIVEMENT",
                "SERAIT supprimée",
                "confirm=true",
                ".backup",
            ],
        ),
    ] {
        let t = tools.iter().find(|t| t.name == name).unwrap();
        let d = t.description.as_deref().unwrap();
        assert!(needles.iter().all(|n| d.contains(n)), "{name}: {d}");
        let schema = serde_json::to_string(&t.input_schema).unwrap();
        assert!(
            ["table", "key", "dry_run", "confirm"]
                .iter()
                .all(|k| schema.contains(k)),
            "{name}"
        );
    }
}
