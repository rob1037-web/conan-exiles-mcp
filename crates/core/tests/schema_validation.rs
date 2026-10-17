//! Validation par schéma, de bout en bout dans le noyau: table réelle DT_Purge_Nordheimer
//! (export du Devkit) et schémas de test pour chaque règle.

use conan_core::{
    render_report, validate_table, Config, CsvStore, Rule, Schema, StoreError, Table,
    ValidationError, WriteOptions,
};

const FIXTURE: &str = include_str!("../../../fixtures/DT_Purge_Nordheimer.csv");
const SCHEMA: &str = include_str!("../../../schemas/DT_Purge_Nordheimer.toml");

fn schema() -> Schema {
    Schema::from_toml_str(SCHEMA).unwrap()
}

fn validate(csv: &str) -> Result<usize, Vec<ValidationError>> {
    validate_table(&schema(), &Table::parse(csv.as_bytes()).unwrap()).map(|rows| rows.len())
}

fn rules(csv: &str) -> Vec<Rule> {
    validate(csv).unwrap_err().iter().map(|e| e.rule).collect()
}

const HEADER: &str = "---,SourceName,TargetName,Weight,RowType\n";
fn row(key: &str, source: &str, target: &str, weight: &str, row_type: &str) -> String {
    format!("{key},\"{source}\",\"{target}\",\"{weight}\",\"{row_type}\"\n")
}

#[test]
fn the_real_table_is_valid_against_its_schema() {
    assert_eq!(validate(FIXTURE), Ok(5));
    let s = schema();
    assert_eq!(s.key_column, "---");
    let names: Vec<_> = s.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["SourceName", "TargetName", "Weight", "RowType"]);
}

#[test]
fn header_must_match_exactly_names_and_order() {
    let body = row("Trash1", "N", "T", "1.000000", "WeightedSpawnTable");
    let cases: Vec<(String, Rule)> = vec![
        // colonne manquante
        (
            format!(
                "---,SourceName,TargetName,Weight\n{}",
                "Trash1,\"N\",\"T\",\"1\"\n"
            ),
            Rule::MissingColumn,
        ),
        // colonne inattendue
        (
            format!(
                "---,SourceName,TargetName,Weight,RowType,Extra\n{}",
                "Trash1,\"N\",\"T\",\"1\",\"WeightedSpawnTable\",\"x\"\n"
            ),
            Rule::UnknownColumn,
        ),
        // ordre différent (celui que le Devkit n'exporte pas)
        (
            format!(
                "---,SourceName,TargetName,RowType,Weight\n{}",
                "Trash1,\"N\",\"T\",\"WeightedSpawnTable\",\"1\"\n"
            ),
            Rule::ColumnOrder,
        ),
        // en-tête de la colonne clé
        (
            format!("Name,SourceName,TargetName,Weight,RowType\n{body}"),
            Rule::KeyHeaderMismatch,
        ),
        // casse
        (
            format!("---,sourcename,TargetName,Weight,RowType\n{body}"),
            Rule::UnknownColumn,
        ),
        // doublon
        (
            format!(
                "---,SourceName,SourceName,Weight,RowType\n{}",
                "Trash1,\"N\",\"T\",\"1\",\"WeightedSpawnTable\"\n"
            ),
            Rule::DuplicateColumn,
        ),
    ];
    for (csv, expected_rule) in cases {
        let errors = validate(&csv).unwrap_err();
        assert!(
            errors.iter().any(|e| e.rule == expected_rule),
            "{csv}: {errors:?}"
        );
        assert!(
            errors
                .iter()
                .all(|e| e.rule.is_schema_drift() && e.line == 1),
            "{csv}: {errors:?}"
        );
    }
}

#[test]
fn drift_stops_value_validation_and_is_flagged() {
    // en-tête dérivé ET valeur invalide: seule la dérive est rapportée (les lignes ne sont pas lisibles)
    let csv = format!(
        "---,SourceName,TargetName,RowType,Weight\n{}",
        "Trash1,\"N\",\"T\",\"Bad\",\"abc\"\n"
    );
    assert_eq!(rules(&csv), [Rule::ColumnOrder]);
}

#[test]
fn every_value_rule_is_reported_with_key_column_value_and_rule() {
    let csv = format!(
        "{HEADER}{}{}{}{}",
        row("Trash1", "N", "T", "abc", "WeightedSpawnTable"), // WrongType
        row("Trash2", "", "T", "1.0", "WeightedSpawnTable"),  // RequiredEmpty
        row("Trash3", "N", "T", "1.0", "Inconnu"),            // NotInEnum
        row("Trash4", "N", "T", "NaN", "WeightedSpawnTable"), // WrongType (non fini)
    );
    let errors = validate(&csv).unwrap_err();
    let summary: Vec<_> = errors
        .iter()
        .map(|e| {
            (
                e.line,
                e.key.as_deref().unwrap(),
                e.column.as_deref().unwrap(),
                e.received.as_deref().unwrap(),
                e.rule,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (2, "Trash1", "Weight", "abc", Rule::WrongType),
            (3, "Trash2", "SourceName", "", Rule::RequiredEmpty),
            (4, "Trash3", "RowType", "Inconnu", Rule::NotInEnum),
            (5, "Trash4", "Weight", "NaN", Rule::WrongType),
        ]
    );
    assert!(errors[2].expected.contains("WeightedSpawnTable"));
    assert!(errors.iter().all(|e| !e.rule.is_schema_drift()));
}

#[test]
fn all_errors_are_listed_not_just_the_first() {
    let mut csv = String::from(HEADER);
    for i in 0..30 {
        csv.push_str(&row(&format!("T{i}"), "N", "T", "x", "Y")); // 2 erreurs par ligne
    }
    let errors = validate(&csv).unwrap_err();
    assert_eq!(errors.len(), 60);
    let text = render_report(&errors, 0);
    assert_eq!(text.lines().count(), 60);
    assert!(render_report(&errors, 5).contains("… et 55 autre(s) erreur(s)"));
}

#[test]
fn duplicate_keys_are_detected_ignoring_ascii_case() {
    let csv = format!(
        "{HEADER}{}{}{}",
        row("Trash1", "N", "T", "1.0", "WeightedSpawnTable"),
        row("Trash2", "N", "T", "1.0", "WeightedSpawnTable"),
        row("TRASH1", "N", "T", "1.0", "WeightedSpawnTable"),
    );
    let errors = validate(&csv).unwrap_err();
    assert_eq!(errors.len(), 1);
    let e = &errors[0];
    assert_eq!(
        (e.rule, e.line, e.key.as_deref()),
        (Rule::DuplicateKey, 4, Some("TRASH1"))
    );
    assert!(e.expected.contains("ligne 2") && e.expected.contains("Trash1"));
    // une clé exactement répétée aussi
    let csv = format!(
        "{HEADER}{0}{0}",
        row("A", "N", "T", "1", "WeightedSpawnTable")
    );
    assert_eq!(rules(&csv), [Rule::DuplicateKey]);
}

#[test]
fn empty_key_is_reported() {
    let csv = format!("{HEADER}{}", row("", "N", "T", "1", "WeightedSpawnTable"));
    assert_eq!(rules(&csv), [Rule::EmptyKey]);
}

const ALL_TYPES: &str = r#"
name = "T"
key_column = "---"
[[fields]]
name = "Count"
type = "int"
min = 0
max = 10
[[fields]]
name = "Ratio"
type = "float"
min = 0
max = 1
[[fields]]
name = "Flag"
type = "bool"
[[fields]]
name = "Label"
type = "string"
min_len = 2
max_len = 4
[[fields]]
name = "Kind"
type = "enum"
values = ["A", "B"]
"#;

fn all_types(values: [&str; 5]) -> Result<usize, Vec<ValidationError>> {
    let s = Schema::from_toml_str(ALL_TYPES).unwrap();
    let csv = format!(
        "---,Count,Ratio,Flag,Label,Kind\nk,\"{}\",\"{}\",\"{}\",\"{}\",\"{}\"\n",
        values[0], values[1], values[2], values[3], values[4]
    );
    validate_table(&s, &Table::parse(csv.as_bytes()).unwrap()).map(|r| r.len())
}

#[test]
fn each_type_accepts_valid_values_and_rejects_invalid_ones() {
    assert_eq!(all_types(["5", "0.5", "true", "abc", "A"]), Ok(1));
    assert_eq!(all_types(["", "", "", "", ""]), Ok(1)); // champs facultatifs vides
    assert_eq!(all_types(["0", "1", "FALSE", "ab", "B"]), Ok(1));
    let rule = |v: [&str; 5]| {
        let e = all_types(v).unwrap_err();
        assert_eq!(e.len(), 1, "{v:?}: {e:?}");
        e[0].rule
    };
    assert_eq!(rule(["x", "0.5", "true", "abc", "A"]), Rule::WrongType); // int
    assert_eq!(rule(["1.5", "0.5", "true", "abc", "A"]), Rule::WrongType);
    assert_eq!(rule(["11", "0.5", "true", "abc", "A"]), Rule::OutOfRange);
    assert_eq!(rule(["-1", "0.5", "true", "abc", "A"]), Rule::OutOfRange);
    assert_eq!(rule(["5", "zero", "true", "abc", "A"]), Rule::WrongType); // float
    assert_eq!(rule(["5", "1.5", "true", "abc", "A"]), Rule::OutOfRange);
    assert_eq!(rule(["5", "0.5", "oui", "abc", "A"]), Rule::WrongType); // bool
    assert_eq!(rule(["5", "0.5", "1", "abc", "A"]), Rule::WrongType);
    assert_eq!(rule(["5", "0.5", "true", "a", "A"]), Rule::OutOfRange); // string trop court
    assert_eq!(rule(["5", "0.5", "true", "abcde", "A"]), Rule::OutOfRange);
    assert_eq!(rule(["5", "0.5", "true", "abc", "a"]), Rule::NotInEnum); // casse
    assert_eq!(rule(["5", "0.5", "true", "abc", "C"]), Rule::NotInEnum);
}

// --- écriture validée ---------------------------------------------------------------

struct Env {
    _tmp: tempfile::TempDir,
    store: CsvStore,
    csv_root: std::path::PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let mod_root = tmp.path().join("mod");
    std::fs::create_dir_all(&mod_root).unwrap();
    let csv_root = tmp.path().join("csv");
    let store = CsvStore::open(&Config::new(&mod_root, Some(csv_root.clone()))).unwrap();
    Env {
        store,
        csv_root: store_root(&csv_root),
        _tmp: tmp,
    }
}

fn store_root(p: &std::path::Path) -> std::path::PathBuf {
    p.canonicalize().unwrap()
}

fn confirmed() -> WriteOptions {
    WriteOptions {
        dry_run: false,
        confirm: true,
    }
}

#[test]
fn dry_run_returns_diff_and_report_and_writes_nothing() {
    let e = env();
    let s = schema();
    let valid = e
        .store
        .write_with_schema(
            &s,
            "t.csv",
            FIXTURE.as_bytes(),
            WriteOptions {
                dry_run: true,
                confirm: false,
            },
        )
        .unwrap();
    assert!(valid.report.is_empty() && !valid.outcome.applied);
    assert_eq!(valid.outcome.diff.added.len(), 5);

    let bad = FIXTURE.replace("\"10.000000\"", "\"dix\"");
    let invalid = e
        .store
        .write_with_schema(
            &s,
            "t.csv",
            bad.as_bytes(),
            WriteOptions {
                dry_run: true,
                confirm: false,
            },
        )
        .unwrap();
    assert_eq!(invalid.report.len(), 1);
    assert_eq!(invalid.report[0].rule, Rule::WrongType);
    assert_eq!(
        invalid.outcome.diff.added.len(),
        5,
        "le diff est fourni malgré l'invalidité"
    );
    assert!(!e.csv_root.join("t.csv").exists());
}

#[test]
fn real_write_is_refused_when_invalid_and_never_forced() {
    let e = env();
    let s = schema();
    let bad = FIXTURE.replace("\"WeightedSpawnTable\"", "\"Bogus\"");
    let err = e
        .store
        .write_with_schema(&s, "t.csv", bad.as_bytes(), confirmed())
        .unwrap_err();
    assert_eq!(err.code(), "ValidationFailed");
    assert!(matches!(&err, StoreError::ValidationFailed { errors, .. } if errors.len() == 5));
    assert!(err.to_string().contains("rien n'a été écrit"));
    assert!(!e.csv_root.join("t.csv").exists());

    // dérive de schéma: code dédié
    let drift = FIXTURE.replacen("RowType", "RowKind", 1);
    let err = e
        .store
        .write_with_schema(&s, "t.csv", drift.as_bytes(), confirmed())
        .unwrap_err();
    assert_eq!(err.code(), "SchemaDrift");
    assert!(!e.csv_root.join("t.csv").exists());
}

#[test]
fn valid_write_keeps_the_existing_backup_confirm_and_atomic_behavior() {
    let e = env();
    let s = schema();
    let out = e
        .store
        .write_with_schema(&s, "t.csv", FIXTURE.as_bytes(), confirmed())
        .unwrap();
    assert!(out.outcome.applied && out.report.is_empty());
    assert_eq!(
        std::fs::read_to_string(e.csv_root.join("t.csv")).unwrap(),
        FIXTURE
    );

    // remplacement: sans confirm refusé, avec confirm sauvegardé
    let v2 = format!(
        "{FIXTURE}{}",
        row(
            "Trash5",
            "Nordheimer",
            "PurgeNew_X",
            "2.000000",
            "WeightedSpawnTable"
        )
    );
    let err = e
        .store
        .write_with_schema(&s, "t.csv", v2.as_bytes(), WriteOptions::default())
        .unwrap_err();
    assert!(matches!(err, StoreError::AlreadyExists { .. }));
    let out = e
        .store
        .write_with_schema(&s, "t.csv", v2.as_bytes(), confirmed())
        .unwrap();
    let backup = out.outcome.backup.unwrap();
    assert_eq!(std::fs::read_to_string(backup).unwrap(), FIXTURE);
    assert_eq!(
        std::fs::read_to_string(e.csv_root.join("t.csv")).unwrap(),
        v2
    );
    assert_eq!(out.outcome.diff.added.len(), 1);
}

#[test]
fn rows_from_a_client_are_validated_like_a_csv() {
    let s = schema();
    let rows = vec![
        vec![
            "Trash1".to_string(),
            "N".into(),
            "T".into(),
            "10.000000".into(),
            "WeightedSpawnTable".into(),
        ],
        vec![
            "Trash2".to_string(),
            "N".into(),
            "T".into(),
            "x".into(),
            "WeightedSpawnTable".into(),
        ],
    ];
    let table = Table::for_schema(&s, &rows).unwrap();
    assert_eq!(
        String::from_utf8(table.to_bytes()).unwrap(),
        "---,SourceName,TargetName,Weight,RowType\nTrash1,\"N\",\"T\",\"10.000000\",\"WeightedSpawnTable\"\nTrash2,\"N\",\"T\",\"x\",\"WeightedSpawnTable\"\n"
    );
    let errors = validate_table(&s, &table).unwrap_err();
    assert_eq!((errors[0].line, errors[0].rule), (3, Rule::WrongType));

    // mauvais nombre de valeurs
    let short = vec![vec!["Trash1".to_string(), "N".into()]];
    let errors = Table::for_schema(&s, &short).unwrap_err();
    assert_eq!((errors[0].rule, errors[0].line), (Rule::ColumnCount, 2));
}

#[test]
fn every_versioned_schema_loads_in_a_schema_root() {
    use conan_core::SchemaStore;
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in [
        ("DT_Purge_Nordheimer", SCHEMA),
        (
            "lightning_rod_loot",
            include_str!("../../../schemas/lightning_rod_loot.toml"),
        ),
        ("exemple", include_str!("../../../schemas/exemple.toml")),
    ] {
        std::fs::write(dir.path().join(format!("{name}.toml")), text).unwrap();
    }
    let store = SchemaStore::open(dir.path()).unwrap();
    for entry in store.list().unwrap() {
        let schema = entry
            .status
            .unwrap_or_else(|e| panic!("{}: {e}", entry.file));
        assert_eq!(schema.key_column, "---", "{}", entry.file);
    }
}

// --- format du Devkit: le diff compare les valeurs, l'écriture reproduit le format ---------

fn devkit(csv: &str) -> String {
    let table = Table::parse(csv.as_bytes()).unwrap();
    String::from_utf8(table.to_devkit_format(&schema()).to_bytes()).unwrap()
}

#[test]
fn any_received_format_becomes_the_devkit_format() {
    // l'export réel est déjà au format du Devkit
    assert_eq!(devkit(FIXTURE), FIXTURE);
    // aucun guillemet, entiers à la place des flottants
    let bare = "---,SourceName,TargetName,Weight,RowType\nTrash1,Nordheimer,PurgeNew_Nordheimer_Grunt_1,10,WeightedSpawnTable\n";
    assert_eq!(
        devkit(bare),
        "---,SourceName,TargetName,Weight,RowType\nTrash1,\"Nordheimer\",\"PurgeNew_Nordheimer_Grunt_1\",\"10.000000\",\"WeightedSpawnTable\"\n"
    );
    // tout entre guillemets, CRLF, BOM, pas de retour final
    let messy = "\u{feff}\"---\",\"SourceName\",\"TargetName\",\"Weight\",\"RowType\"\r\n\"Trash1\",\"N\",\"T\",\"2.5\",\"WeightedSpawnTable\"";
    assert_eq!(
        devkit(messy),
        "---,SourceName,TargetName,Weight,RowType\nTrash1,\"N\",\"T\",\"2.500000\",\"WeightedSpawnTable\"\n"
    );
}

#[test]
fn floats_are_written_with_six_decimals_but_never_rounded() {
    let row = |w: &str| format!("{HEADER}Trash1,\"N\",\"T\",\"{w}\",\"WeightedSpawnTable\"\n");
    let weight_of = |csv: String| {
        let t = Table::parse(devkit(&csv).as_bytes()).unwrap();
        t.rows[0].cells[3].text.clone()
    };
    assert_eq!(weight_of(row("20")), "20.000000");
    assert_eq!(weight_of(row("0.5")), "0.500000");
    assert_eq!(weight_of(row("-3")), "-3.000000");
    assert_eq!(weight_of(row("1e2")), "100.000000");
    assert_eq!(weight_of(row("10.000000")), "10.000000");
    // plus de six décimales: conservé tel quel, jamais arrondi
    assert_eq!(weight_of(row("0.1234567")), "0.1234567");
    // texte invalide: laissé tel quel (la validation le signalera)
    assert_eq!(weight_of(row("abc")), "abc");
}

#[test]
fn diff_shows_only_the_real_change_when_values_arrive_without_quotes() {
    let e = env();
    let s = schema();
    e.store
        .write_with_schema(&s, "t.csv", FIXTURE.as_bytes(), confirmed())
        .unwrap();

    // mêmes valeurs, aucun guillemet, Trash1/Weight passe de 10 à 20 (écrit « 20 »)
    let bare = "---,SourceName,TargetName,Weight,RowType\n\
        Trash1,Nordheimer,PurgeNew_Nordheimer_Grunt_1,20,WeightedSpawnTable\n\
        Trash2,Nordheimer,PurgeNew_Nordheimer_Grunt_2,3,WeightedSpawnTable\n\
        Trash3,NordheimerWolf,PurgeNew_Nordheimer_Wolf,5,WeightedSpawnTable\n\
        Trash4,NordheimerGiant,PurgeNew_Nordheimer_FrostGiant_1,1,WeightedSpawnTable\n\
        TrashGeneric,Nordheimer,PurgeNew_Generic_1,1,WeightedSpawnTable\n";
    let dry = e
        .store
        .write_with_schema(
            &s,
            "t.csv",
            bare.as_bytes(),
            WriteOptions {
                dry_run: true,
                confirm: false,
            },
        )
        .unwrap();
    assert!(dry.report.is_empty() && dry.normalized);
    let d = &dry.outcome.diff;
    assert!(d.added.is_empty() && d.removed.is_empty() && !d.header_changed && !d.quoting_changed);
    assert_eq!(d.modified.len(), 1);
    assert_eq!(d.modified[0].key, "Trash1");
    assert_eq!(d.modified[0].changes.len(), 1);
    assert_eq!(
        (
            d.modified[0].changes[0].column.as_str(),
            d.modified[0].changes[0].before.as_str(),
            d.modified[0].changes[0].after.as_str()
        ),
        ("Weight", "10.000000", "20.000000")
    );
    assert_eq!(d.summary(), "0 ajoutée(s), 1 modifiée(s), 0 supprimée(s)");

    // l'écriture réelle garde le format d'origine
    let out = e
        .store
        .write_with_schema(&s, "t.csv", bare.as_bytes(), confirmed())
        .unwrap();
    assert!(out.outcome.applied && out.normalized);
    let written = std::fs::read_to_string(e.csv_root.join("t.csv")).unwrap();
    assert_eq!(written, FIXTURE.replace("\"10.000000\"", "\"20.000000\""));
    assert_eq!(
        std::fs::read_to_string(out.outcome.backup.unwrap()).unwrap(),
        FIXTURE
    );

    // renvoyer le même contenu, quel que soit son format: aucun changement
    let again = e
        .store
        .write_with_schema(&s, "t.csv", bare.as_bytes(), confirmed())
        .unwrap();
    assert!(again.outcome.unchanged && !again.outcome.applied && again.outcome.backup.is_none());
}

#[test]
fn a_canonical_input_is_not_reported_as_normalized() {
    let e = env();
    let out = e
        .store
        .write_with_schema(&schema(), "t.csv", FIXTURE.as_bytes(), confirmed())
        .unwrap();
    assert!(!out.normalized);
}
