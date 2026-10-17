//! Opérations par ligne (add / update / delete) sur des copies temporaires du fixture réel
//! DT_Purge_Nordheimer.

use conan_core::{Config, CsvStore, RowOp, Rule, Schema, StoreError, WriteOptions};
use std::fs;
use std::path::PathBuf;

const FIXTURE: &str = include_str!("../../../fixtures/DT_Purge_Nordheimer.csv");
const SCHEMA: &str = include_str!("../../../schemas/DT_Purge_Nordheimer.toml");
const FILE: &str = "DT_Purge_Nordheimer.csv";

struct Env {
    _tmp: tempfile::TempDir,
    store: CsvStore,
    schema: Schema,
    csv_root: PathBuf,
}

impl Env {
    /// Copie temporaire du fixture dans un CSV_ROOT jetable.
    fn new() -> Env {
        Env::with(FIXTURE)
    }

    fn with(content: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let mod_root = tmp.path().join("mod");
        fs::create_dir_all(&mod_root).unwrap();
        let csv_root = tmp.path().join("csv");
        let store = CsvStore::open(&Config::new(&mod_root, Some(csv_root.clone()))).unwrap();
        let csv_root = csv_root.canonicalize().unwrap();
        fs::write(csv_root.join(FILE), content).unwrap();
        Env {
            _tmp: tmp,
            store,
            schema: Schema::from_toml_str(SCHEMA).unwrap(),
            csv_root,
        }
    }

    fn file(&self) -> String {
        fs::read_to_string(self.csv_root.join(FILE)).unwrap()
    }

    fn run(&self, op: &RowOp, dry_run: bool) -> Result<conan_core::RowOutcome, StoreError> {
        self.store.apply_row_op(
            &self.schema,
            FILE,
            op,
            WriteOptions {
                dry_run,
                confirm: !dry_run,
            },
        )
    }

    fn backups(&self) -> usize {
        match fs::read_dir(self.csv_root.join(".backup")) {
            Ok(d) => d.count(),
            Err(_) => 0,
        }
    }
}

fn vals(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(c, v)| ((*c).to_owned(), (*v).to_owned()))
        .collect()
}

fn add(key: &str, source: &str, target: &str, weight: &str, row_type: &str) -> RowOp {
    RowOp::Add {
        key: key.into(),
        values: vals(&[
            ("SourceName", source),
            ("TargetName", target),
            ("Weight", weight),
            ("RowType", row_type),
        ]),
    }
}

fn update(key: &str, pairs: &[(&str, &str)]) -> RowOp {
    RowOp::Update {
        key: key.into(),
        values: vals(pairs),
    }
}

fn delete(key: &str) -> RowOp {
    RowOp::Delete { key: key.into() }
}

fn line(key: &str, source: &str, target: &str, weight: &str) -> String {
    format!("{key},\"{source}\",\"{target}\",\"{weight}\",\"WeightedSpawnTable\"\n")
}

// --- add_row -------------------------------------------------------------------------

#[test]
fn add_dry_run_then_real_give_the_same_diff_and_the_exact_devkit_bytes() {
    let e = Env::new();
    let op = add(
        "Trash5",
        "Nordheimer",
        "PurgeNew_Nordheimer_Grunt_3",
        "2",
        "WeightedSpawnTable",
    );

    let dry = e.run(&op, true).unwrap();
    assert!(!dry.no_change && !dry.outcome().applied && dry.outcome().backup.is_none());
    assert_eq!(dry.outcome().diff.added.len(), 1);
    assert_eq!(dry.outcome().diff.added[0].key, "Trash5");
    assert!(dry.write.report.is_empty());
    assert_eq!(e.file(), FIXTURE, "dry-run: rien d'écrit");
    assert_eq!(e.backups(), 0);

    let real = e.run(&op, false).unwrap();
    assert!(real.outcome().applied);
    assert_eq!(real.outcome().diff, dry.outcome().diff, "même diff");
    // octet par octet: fixture + la nouvelle ligne en fin, flottant à six décimales
    let expected = format!(
        "{FIXTURE}{}",
        line(
            "Trash5",
            "Nordheimer",
            "PurgeNew_Nordheimer_Grunt_3",
            "2.000000"
        )
    );
    assert_eq!(e.file().as_bytes(), expected.as_bytes());
    // sauvegarde horodatée identique à l'original
    assert_eq!(
        fs::read_to_string(real.outcome().backup.as_ref().unwrap()).unwrap(),
        FIXTURE
    );
    assert_eq!(e.backups(), 1);
}

#[test]
fn add_refuses_an_existing_key_including_case_variants_and_keeps_the_given_case() {
    let e = Env::new();
    for key in ["Trash1", "trash1", "TRASH1", "trashgeneric", "TrashGENERIC"] {
        let err = e
            .run(&add(key, "N", "T", "1", "WeightedSpawnTable"), false)
            .unwrap_err();
        assert!(matches!(err, StoreError::KeyExists { .. }), "{key}: {err}");
        assert_eq!(err.code(), "KeyExists");
        assert!(err.to_string().contains("update_row"));
    }
    assert_eq!(e.file(), FIXTURE);
    assert_eq!(e.backups(), 0);
    // la casse fournie est conservée
    e.run(&add("trASH9", "N", "T", "1", "WeightedSpawnTable"), false)
        .unwrap();
    assert!(e.file().ends_with(&line("trASH9", "N", "T", "1.000000")));
}

#[test]
fn add_rejects_unknown_missing_and_key_columns_even_in_dry_run() {
    let e = Env::new();
    let unknown = RowOp::Add {
        key: "Trash5".into(),
        values: vals(&[
            ("SourceName", "N"),
            ("TargetName", "T"),
            ("Weight", "1"),
            ("RowType", "WeightedSpawnTable"),
            ("Poids", "3"),
        ]),
    };
    let err = e.run(&unknown, true).unwrap_err();
    assert_eq!(err.code(), "BadColumns");
    assert!(
        err.to_string().contains("\"Poids\"")
            && err
                .to_string()
                .contains("SourceName, TargetName, Weight, RowType")
    );
    let casing = RowOp::Add {
        key: "Trash5".into(),
        values: vals(&[
            ("sourcename", "N"),
            ("TargetName", "T"),
            ("Weight", "1"),
            ("RowType", "WeightedSpawnTable"),
        ]),
    };
    let msg = e.run(&casing, true).unwrap_err().to_string();
    assert!(
        msg.contains("casse différente") && msg.contains("manquante"),
        "{msg}"
    );
    let missing = RowOp::Add {
        key: "Trash5".into(),
        values: vals(&[("SourceName", "N")]),
    };
    let msg = e.run(&missing, false).unwrap_err().to_string();
    assert!(
        msg.contains("manquante") && msg.contains("TargetName, Weight, RowType"),
        "{msg}"
    );
    let with_key = RowOp::Add {
        key: "Trash5".into(),
        values: vals(&[("---", "x")]),
    };
    assert_eq!(
        e.run(&with_key, false).unwrap_err().code(),
        "KeyNotEditable"
    );
    assert_eq!(e.file(), FIXTURE);
}

#[test]
fn add_validates_the_whole_resulting_table() {
    let e = Env::new();
    for (op, rule) in [
        (
            add("Trash5", "N", "T", "beaucoup", "WeightedSpawnTable"),
            Rule::WrongType,
        ),
        (add("Trash5", "N", "T", "1", "Inconnu"), Rule::NotInEnum),
        (
            add("Trash5", "", "T", "1", "WeightedSpawnTable"),
            Rule::RequiredEmpty,
        ),
        (add("", "N", "T", "1", "WeightedSpawnTable"), Rule::EmptyKey),
    ] {
        // dry-run: rapport, pas d'erreur ; réel: refus
        let dry = e.run(&op, true).unwrap();
        assert_eq!(dry.write.report.len(), 1, "{op:?}");
        assert_eq!(dry.write.report[0].rule, rule);
        assert_eq!(
            dry.write.report[0].line, 7,
            "la nouvelle ligne est la 7e du fichier"
        );
        let err = e.run(&op, false).unwrap_err();
        assert_eq!(err.code(), "ValidationFailed", "{op:?}");
    }
    assert_eq!(e.file(), FIXTURE);
    assert_eq!(e.backups(), 0);
}

#[test]
fn add_to_a_non_canonical_source_rewrites_the_whole_file_in_devkit_format() {
    // source valide mais sans guillemets ni décimales
    let bare = "---,SourceName,TargetName,Weight,RowType\nTrash1,Nordheimer,PurgeNew_A,10,WeightedSpawnTable\n";
    let e = Env::with(bare);
    let out = e
        .run(&add("Trash2", "N", "T", "3", "WeightedSpawnTable"), false)
        .unwrap();
    assert!(out.outcome().applied);
    assert_eq!(
        e.file(),
        "---,SourceName,TargetName,Weight,RowType\nTrash1,\"Nordheimer\",\"PurgeNew_A\",\"10.000000\",\"WeightedSpawnTable\"\nTrash2,\"N\",\"T\",\"3.000000\",\"WeightedSpawnTable\"\n"
    );
}

// --- update_row ----------------------------------------------------------------------

#[test]
fn update_merges_partial_fields_keeps_order_and_the_stored_key_case() {
    let e = Env::new();
    let op = update("trash2", &[("Weight", "25")]); // casse différente de Trash2
    let dry = e.run(&op, true).unwrap();
    assert_eq!(dry.key, "Trash2");
    assert_eq!(dry.outcome().diff.modified.len(), 1);
    assert_eq!(dry.outcome().diff.modified[0].key, "Trash2");
    assert_eq!(
        dry.outcome().diff.modified[0].changes.len(),
        1,
        "un seul champ touché"
    );
    assert_eq!(e.file(), FIXTURE);

    let real = e.run(&op, false).unwrap();
    assert_eq!(real.outcome().diff, dry.outcome().diff);
    assert_eq!(
        e.file().as_bytes(),
        FIXTURE.replace("\"3.000000\"", "\"25.000000\"").as_bytes()
    );
    assert_eq!(
        fs::read_to_string(real.outcome().backup.as_ref().unwrap()).unwrap(),
        FIXTURE
    );
    // plusieurs champs d'un coup, les autres colonnes intactes
    e.run(
        &update(
            "Trash4",
            &[("TargetName", "PurgeNew_Autre"), ("Weight", "0.5")],
        ),
        false,
    )
    .unwrap();
    let f = e.file();
    assert!(
        f.contains(
            "Trash4,\"NordheimerGiant\",\"PurgeNew_Autre\",\"0.500000\",\"WeightedSpawnTable\"\n"
        ),
        "{f}"
    );
    let order: Vec<&str> = f
        .lines()
        .skip(1)
        .map(|l| l.split(',').next().unwrap())
        .collect();
    assert_eq!(
        order,
        ["Trash1", "Trash2", "Trash3", "Trash4", "TrashGeneric"]
    );
}

#[test]
fn update_refuses_a_missing_key_the_key_column_and_unknown_columns() {
    let e = Env::new();
    let err = e
        .run(&update("Trash99", &[("Weight", "1")]), false)
        .unwrap_err();
    assert_eq!(err.code(), "KeyNotFound");
    assert!(err.to_string().contains("Trash1, Trash2"), "{err}");
    assert_eq!(
        e.run(&update("Trash1", &[("---", "Autre")]), false)
            .unwrap_err()
            .code(),
        "KeyNotEditable"
    );
    assert_eq!(
        e.run(
            &update("Trash1", &[("Weight", "1"), ("---", "Autre")]),
            true
        )
        .unwrap_err()
        .code(),
        "KeyNotEditable"
    );
    let err = e
        .run(&update("Trash1", &[("Poids", "1")]), false)
        .unwrap_err();
    assert_eq!(err.code(), "BadColumns");
    assert!(err.to_string().contains("\"Poids\""));
    assert_eq!(e.file(), FIXTURE);
    assert_eq!(e.backups(), 0);
}

#[test]
fn update_with_invalid_values_reports_in_dry_run_and_refuses_for_real() {
    let e = Env::new();
    for (pairs, rule) in [
        (vec![("Weight", "lourd")], Rule::WrongType),
        (vec![("RowType", "Inconnu")], Rule::NotInEnum),
        (vec![("SourceName", "")], Rule::RequiredEmpty),
    ] {
        let dry = e.run(&update("Trash1", &pairs), true).unwrap();
        assert_eq!(dry.write.report[0].rule, rule);
        assert_eq!(dry.write.report[0].key.as_deref(), Some("Trash1"));
        assert_eq!(
            e.run(&update("Trash1", &pairs), false).unwrap_err().code(),
            "ValidationFailed"
        );
    }
    assert_eq!(e.file(), FIXTURE);
    assert_eq!(e.backups(), 0);
}

#[test]
fn update_that_changes_no_value_writes_nothing_and_makes_no_backup() {
    let e = Env::new();
    let before = fs::metadata(e.csv_root.join(FILE))
        .unwrap()
        .modified()
        .unwrap();
    // même valeur écrite autrement ("10" = "10.000000"), valeur identique, et fusion vide
    for pairs in [
        vec![("Weight", "10")],
        vec![("Weight", "10.000000")],
        vec![("SourceName", "Nordheimer"), ("Weight", "1e1")],
        vec![],
    ] {
        for dry_run in [true, false] {
            let out = e.run(&update("Trash1", &pairs), dry_run).unwrap();
            assert!(
                out.no_change && !out.outcome().applied && out.outcome().backup.is_none(),
                "{pairs:?}"
            );
            assert!(out.outcome().diff.added.is_empty() && out.outcome().diff.modified.is_empty());
        }
    }
    assert_eq!(e.file(), FIXTURE);
    assert_eq!(
        fs::metadata(e.csv_root.join(FILE))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
    assert_eq!(e.backups(), 0);
}

// --- delete_row ----------------------------------------------------------------------

#[test]
fn delete_shows_the_row_in_dry_run_and_removes_it_for_real() {
    let e = Env::new();
    let dry = e.run(&delete("TRASH3"), true).unwrap();
    assert_eq!(dry.key, "Trash3");
    assert_eq!(dry.outcome().diff.removed.len(), 1);
    assert_eq!(
        dry.outcome().diff.removed[0].cells,
        [
            "Trash3",
            "NordheimerWolf",
            "PurgeNew_Nordheimer_Wolf",
            "5.000000",
            "WeightedSpawnTable"
        ]
    );
    assert!(dry
        .outcome()
        .diff
        .render()
        .contains("- Trash3 | Trash3, NordheimerWolf"));
    assert_eq!(e.file(), FIXTURE);

    let real = e.run(&delete("TRASH3"), false).unwrap();
    assert_eq!(real.outcome().diff, dry.outcome().diff);
    let expected = FIXTURE.replace(
        &line(
            "Trash3",
            "NordheimerWolf",
            "PurgeNew_Nordheimer_Wolf",
            "5.000000",
        ),
        "",
    );
    assert_eq!(e.file().as_bytes(), expected.as_bytes());
    assert_eq!(
        fs::read_to_string(real.outcome().backup.as_ref().unwrap()).unwrap(),
        FIXTURE
    );
    // supprimer à nouveau: la clé n'existe plus
    assert_eq!(
        e.run(&delete("Trash3"), false).unwrap_err().code(),
        "KeyNotFound"
    );
}

#[test]
fn delete_refuses_a_missing_key_and_can_empty_the_table() {
    let e = Env::new();
    assert_eq!(
        e.run(&delete("Trash99"), true).unwrap_err().code(),
        "KeyNotFound"
    );
    for k in ["Trash1", "Trash2", "Trash3", "Trash4", "TrashGeneric"] {
        e.run(&delete(k), false).unwrap();
    }
    assert_eq!(e.file(), "---,SourceName,TargetName,Weight,RowType\n");
    assert_eq!(
        e.run(&delete("Trash1"), false).unwrap_err().code(),
        "KeyNotFound"
    );
}

// --- source invalide -----------------------------------------------------------------

#[test]
fn an_invalid_source_csv_blocks_every_operation_without_repair() {
    let ops = [
        add("Trash9", "N", "T", "1", "WeightedSpawnTable"),
        update("Trash1", &[("Weight", "1")]),
        delete("Trash1"),
    ];
    let broken = [
        FIXTURE.replace("\"5.000000\"", "\"cinq\""), // valeur invalide
        FIXTURE.replacen("RowType", "RowKind", 1),   // dérive d'en-tête
        format!("{FIXTURE}Trash1,\"N\",\"T\",\"1.000000\",\"WeightedSpawnTable\"\n"), // clé en double
    ];
    for content in &broken {
        let e = Env::with(content);
        for op in &ops {
            for dry_run in [true, false] {
                let err = e.run(op, dry_run).unwrap_err();
                assert_eq!(err.code(), "SourceInvalid", "{op:?}: {err}");
                assert!(
                    err.to_string().contains("validate_rows")
                        && err.to_string().contains("aucune réparation")
                );
            }
        }
        assert_eq!(e.file(), *content, "aucune réparation");
        assert_eq!(e.backups(), 0);
    }
    // illisible comme CSV
    let e = Env::with("\"jamais ferme");
    assert_eq!(
        e.run(&ops[2], false).unwrap_err().code(),
        "ExistingUnreadable"
    );
    assert_eq!(e.file(), "\"jamais ferme");
    // fichier absent
    let e = Env::new();
    fs::remove_file(e.csv_root.join(FILE)).unwrap();
    assert_eq!(e.run(&ops[0], false).unwrap_err().code(), "NotFound");
}

#[test]
fn row_operations_keep_the_read_only_roots_untouched() {
    // CSV_ROOT parent du mod: un chemin vers le mod reste refusé
    let tmp = tempfile::tempdir().unwrap();
    let mod_root = tmp.path().join("mod");
    fs::create_dir_all(&mod_root).unwrap();
    fs::write(mod_root.join("m.csv"), FIXTURE).unwrap();
    let store = CsvStore::open(&Config::new(&mod_root, Some(tmp.path().to_path_buf()))).unwrap();
    let schema = Schema::from_toml_str(SCHEMA).unwrap();
    let err = store
        .apply_row_op(
            &schema,
            "mod/m.csv",
            &delete("Trash1"),
            WriteOptions {
                dry_run: false,
                confirm: true,
            },
        )
        .unwrap_err();
    assert_eq!(err.code(), "ModReadOnly");
    assert_eq!(fs::read_to_string(mod_root.join("m.csv")).unwrap(), FIXTURE);
}
