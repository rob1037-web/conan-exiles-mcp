//! E3 (recette 4b, 7.4): un `NSLOCTEXT(...)` dont le texte contient des guillemets bruts s'importe sans
//! erreur mais Unreal l'affiche brut. La validation (donc `validate_rows`, les dry-runs et l'écriture
//! réelle) doit le refuser avant l'écriture. Forme exacte observée sur l'export réel: `\"`, `\'`, `\r\n`.

use std::fs;

use conan_core::{
    validate_table, Config, CsvStore, RowOp, Rule, Schema, StoreError, Table, WriteOptions,
};

const UTF16: &[u8] = include_bytes!("../../../fixtures/encodings/lore_utf16le_bom_crlf.csv");
const UTF8_BOM: &[u8] = include_bytes!("../../../fixtures/encodings/lore_utf8_bom_crlf.csv");
const ASCII: &[u8] = include_bytes!("../../../fixtures/encodings/lore_ascii_lf.csv");

const SCHEMA: &str = r#"
name = "DT_Lore"
key_column = "---"

[[fields]]
name = "Name"
type = "string"

[[fields]]
name = "Text"
type = "string"

[[fields]]
name = "Category"
type = "string"
"#;
const FILE: &str = "DT_Lore.csv";

/// Le cas E3: guillemets bruts dans le texte (ce que produit un CSV doublé mal compris).
const BAD: &str = r#"NSLOCTEXT("", "Lore_900_Text", "Il dit "non" puis part")"#;
const GOOD: &str =
    r#"NSLOCTEXT("", "Lore_900_Text", "Il dit \"non\" puis part, c\'est fini.\r\nFin.")"#;

fn schema() -> Schema {
    Schema::from_toml_str(SCHEMA).unwrap()
}

struct Env {
    _tmp: tempfile::TempDir,
    store: CsvStore,
    root: std::path::PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let mod_root = tmp.path().join("mod");
        fs::create_dir_all(&mod_root).unwrap();
        let csv = tmp.path().join("csv");
        let store = CsvStore::open(&Config::new(&mod_root, Some(csv.clone()))).unwrap();
        let root = csv.canonicalize().unwrap();
        fs::write(root.join(FILE), UTF16).unwrap();
        Env {
            _tmp: tmp,
            store,
            root,
        }
    }

    fn untouched(&self) -> bool {
        fs::read(self.root.join(FILE)).unwrap() == UTF16 && !self.root.join(".backup").exists()
    }
}

fn add(text: &str) -> RowOp {
    RowOp::Add {
        key: "Lore_900".into(),
        values: vec![
            (
                "Name".into(),
                r#"NSLOCTEXT("", "Lore_900_Name", "Nom")"#.into(),
            ),
            ("Text".into(), text.into()),
            ("Category".into(), "Récit".into()),
        ],
    }
}

fn run(env: &Env, op: &RowOp, dry_run: bool) -> Result<conan_core::RowOutcome, StoreError> {
    env.store.apply_row_op(
        &schema(),
        FILE,
        op,
        WriteOptions {
            dry_run,
            confirm: !dry_run,
        },
    )
}

#[test]
fn the_real_forms_in_the_fixtures_validate_in_every_encoding() {
    for bytes in [UTF16, UTF8_BOM, ASCII] {
        let t = Table::parse(bytes).unwrap();
        assert!(validate_table(&schema(), &t).is_ok());
    }
}

#[test]
fn a_malformed_nsloctext_is_a_validation_error_with_line_key_column_and_the_expected_form() {
    let mut t = Table::parse(UTF16).unwrap();
    t.rows[1].cells[2].text = BAD.into();
    let errors = validate_table(&schema(), &t).unwrap_err();
    assert_eq!(errors.len(), 1);
    let e = &errors[0];
    assert_eq!(e.rule, Rule::MalformedLocText);
    assert_eq!(e.rule.code(), "MalformedLocText");
    assert!(!e.rule.is_schema_drift());
    assert_eq!(
        (e.line, e.key.as_deref(), e.column.as_deref()),
        (3, Some("Lore_002"), Some("Text"))
    );
    assert_eq!(e.received.as_deref(), Some(BAD));
    for needle in [
        "trois littéraux",
        "\\\"",
        "\\'",
        "\\r\\n",
        "attendu `)`",
        "position",
    ] {
        assert!(e.expected.contains(needle), "{needle}: {}", e.expected);
    }
}

#[test]
fn add_row_with_raw_quotes_is_reported_in_dry_run_and_refused_for_real_without_touching_the_file() {
    let env = Env::new();
    let dry = run(&env, &add(BAD), true).unwrap();
    assert_eq!(dry.write.report.len(), 1);
    assert_eq!(dry.write.report[0].rule, Rule::MalformedLocText);
    assert!(env.untouched());

    let e = run(&env, &add(BAD), false).unwrap_err();
    let StoreError::ValidationFailed { errors, .. } = &e else {
        panic!("{e}")
    };
    assert_eq!(errors[0].rule, Rule::MalformedLocText);
    assert_eq!(e.code(), "ValidationFailed");
    assert!(env.untouched(), "rien d'écrit, aucune sauvegarde");

    // la forme correcte (\" \' \r\n) passe, en UTF-16 conservé
    let ok = run(&env, &add(GOOD), false).unwrap();
    assert!(ok.write.report.is_empty() && ok.write.outcome.applied);
    let written = fs::read(env.root.join(FILE)).unwrap();
    assert!(written.starts_with(&[0xFF, 0xFE]));
    let t = Table::parse(&written).unwrap();
    assert_eq!(t.rows.last().unwrap().cells[2].text, GOOD);
}

#[test]
fn update_row_and_write_with_schema_refuse_a_malformed_value_too() {
    let env = Env::new();
    let update = RowOp::Update {
        key: "Lore_001".into(),
        values: vec![("Text".into(), BAD.into())],
    };
    let e = run(&env, &update, false).unwrap_err();
    assert!(matches!(e, StoreError::ValidationFailed { .. }), "{e}");
    assert!(env.untouched());

    // write_datatable: contenu complet envoyé par Claude, avec des guillemets bruts
    let mut table = Table::parse(UTF16).unwrap();
    table.rows[0].cells[2].text = BAD.into();
    let mut sent = table.clone();
    sent.encoding = conan_core::Encoding::Utf8;
    sent.newline = conan_core::Newline::Lf;
    let w = env
        .store
        .write_with_schema(
            &schema(),
            FILE,
            &sent.to_bytes(),
            WriteOptions {
                dry_run: true,
                confirm: false,
            },
        )
        .unwrap();
    assert_eq!(w.report.len(), 1);
    assert_eq!(w.report[0].rule, Rule::MalformedLocText);
    let e = env
        .store
        .write_with_schema(
            &schema(),
            FILE,
            &sent.to_bytes(),
            WriteOptions {
                dry_run: false,
                confirm: true,
            },
        )
        .unwrap_err();
    assert!(matches!(e, StoreError::ValidationFailed { .. }), "{e}");
    assert!(env.untouched());
}

#[test]
fn only_nsloctext_values_of_string_columns_are_checked() {
    let mut t = Table::parse(UTF16).unwrap();
    // LOCTABLE (présent dans l'export réel), texte brut et cellule vide: hors périmètre
    t.rows[0].cells[1].text = r#"LOCTABLE("/Game/Items/Strings/ST_X", "Key")"#.into();
    t.rows[0].cells[2].text = "Texte brut avec \"guillemets\" et, virgules".into();
    t.rows[0].cells[3].text = String::new();
    assert!(validate_table(&schema(), &t).is_ok());
    // un champ `raw` n'est jamais vérifié
    let raw_schema = Schema::from_toml_str(&SCHEMA.replace(
        "name = \"Text\"\ntype = \"string\"",
        "name = \"Text\"\ntype = \"raw\"",
    ))
    .unwrap();
    t.rows[0].cells[2].text = BAD.into();
    assert!(validate_table(&raw_schema, &t).is_ok());
    assert!(validate_table(&schema(), &t).is_err());
}
