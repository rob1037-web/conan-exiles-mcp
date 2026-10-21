//! Encodages des CSV exportés par le Devkit: UTF-16 LE avec BOM (dès qu'un texte n'est pas ASCII),
//! UTF-8 avec BOM, ASCII. Lecture, réécriture et opérations par ligne doivent conserver l'encodage,
//! le BOM et les fins de ligne du fichier existant, à l'octet près.
//!
//! Les fixtures de `fixtures/encodings/` sont synthétiques (voir leur README) : colonnes
//! `---,Name,Text,Category`, FText `NSLOCTEXT("", "clé", "texte")` (guillemets doublés), virgules,
//! accents, fins de ligne CRLF (LF pour l'ASCII).

use std::fs;
use std::path::PathBuf;

use conan_core::{
    Config, CsvError, CsvStore, Encoding, Newline, RowOp, Schema, StoreError, Table, WriteOptions,
};

const UTF16: &[u8] = include_bytes!("../../../fixtures/encodings/lore_utf16le_bom_crlf.csv");
const UTF8_BOM: &[u8] = include_bytes!("../../../fixtures/encodings/lore_utf8_bom_crlf.csv");
const ASCII: &[u8] = include_bytes!("../../../fixtures/encodings/lore_ascii_lf.csv");

const SCHEMA: &str = r#"
name = "DT_Lore"
description = "Table de textes (fixture d'encodage)"
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

/// Les trois cas: (fixture, encodage, fins de ligne).
fn cases() -> [(&'static [u8], Encoding, Newline); 3] {
    [
        (UTF16, Encoding::Utf16LeBom, Newline::CrLf),
        (UTF8_BOM, Encoding::Utf8Bom, Newline::CrLf),
        (ASCII, Encoding::Utf8, Newline::Lf),
    ]
}

struct Env {
    _tmp: tempfile::TempDir,
    store: CsvStore,
    schema: Schema,
    csv_root: PathBuf,
}

impl Env {
    fn with(content: &[u8]) -> Env {
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

    fn file(&self) -> Vec<u8> {
        fs::read(self.csv_root.join(FILE)).unwrap()
    }

    fn backups(&self) -> Vec<PathBuf> {
        match fs::read_dir(self.csv_root.join(".backup")) {
            Ok(_) => walk(&self.csv_root.join(".backup")),
            Err(_) => Vec::new(),
        }
    }

    fn row_op(&self, op: &RowOp, dry_run: bool) -> Result<conan_core::RowOutcome, StoreError> {
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
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

fn vals(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(c, v)| ((*c).to_owned(), (*v).to_owned()))
        .collect()
}

/// Le texte d'une fixture, décodé, au format que Claude enverrait (UTF-8, fins de ligne LF).
fn as_client_text(table: &Table) -> String {
    let mut t = table.clone();
    t.encoding = Encoding::Utf8;
    t.newline = Newline::Lf;
    String::from_utf8(t.to_bytes()).unwrap()
}

#[test]
fn fixtures_are_detected_and_their_texts_are_intact() {
    for (bytes, encoding, newline) in cases() {
        let t = Table::parse(bytes).unwrap();
        assert_eq!(t.encoding, encoding);
        assert_eq!(t.newline, newline);
        assert!(t.final_newline);
        assert_eq!(t.header.cells[0].text, "---");
        let name = &t.rows[0].cells[1].text;
        assert!(
            name.starts_with("NSLOCTEXT(\"\", \"Lore_001_Name\", \""),
            "{name}"
        );
        // virgule et guillemets dans le texte, guillemets doublés du CSV ramenés à un seul
        let text = &t.rows[0].cells[2].text;
        assert!(
            text.contains("\"no\", ") || text.contains("\"non\", "),
            "{text}"
        );
    }
    let t = Table::parse(UTF16).unwrap();
    assert_eq!(
        t.rows[0].cells[1].text,
        "NSLOCTEXT(\"\", \"Lore_001_Name\", \"Épée, longue\")"
    );
    assert_eq!(
        t.rows[1].cells[2].text,
        "NSLOCTEXT(\"\", \"Lore_002_Text\", \"Forgé à Æsgard: « gloire, honneur ».\")"
    );
}

#[test]
fn parse_then_serialize_is_identical_to_the_byte_for_every_encoding() {
    for (bytes, ..) in cases() {
        assert_eq!(Table::parse(bytes).unwrap().to_bytes(), bytes);
    }
}

#[test]
fn rewriting_the_content_read_without_modification_changes_nothing() {
    for (bytes, encoding, _) in cases() {
        let env = Env::with(bytes);
        // lecture puis réécriture à l'identique, comme read_datatable puis write_datatable
        let read = env.store.read_csv(FILE).unwrap();
        assert_eq!(read, bytes);
        let w = env
            .store
            .write_with_schema(
                &env.schema,
                FILE,
                &read,
                WriteOptions {
                    dry_run: false,
                    confirm: true,
                },
            )
            .unwrap();
        assert!(w.outcome.unchanged && !w.outcome.applied, "{encoding:?}");
        assert!(
            !w.normalized,
            "{encoding:?}: contenu déjà au format du fichier"
        );
        assert!(w.report.is_empty());
        assert_eq!(w.outcome.encoding, encoding);
        assert_eq!(env.file(), bytes, "{encoding:?}");
        assert!(env.backups().is_empty());
    }
}

#[test]
fn the_same_content_sent_as_utf8_lf_is_written_back_in_the_existing_format() {
    // Claude envoie du texte UTF-8 avec des fins de ligne LF: le fichier reste identique à l'octet près.
    for (bytes, encoding, newline) in cases() {
        let env = Env::with(bytes);
        let sent = as_client_text(&Table::parse(bytes).unwrap());
        assert!(!sent.contains('\r'));
        let w = env
            .store
            .write_with_schema(
                &env.schema,
                FILE,
                sent.as_bytes(),
                WriteOptions {
                    dry_run: false,
                    confirm: true,
                },
            )
            .unwrap();
        assert!(w.outcome.unchanged, "{encoding:?}: {:?}", w.outcome.diff);
        assert_eq!((w.outcome.encoding, w.outcome.newline), (encoding, newline));
        assert_eq!(env.file(), bytes);
        assert!(env.backups().is_empty());
    }
}

#[test]
fn a_real_edit_keeps_encoding_bom_and_line_endings_and_backs_up_the_original() {
    for (bytes, encoding, newline) in cases() {
        let env = Env::with(bytes);
        let table = Table::parse(bytes).unwrap();
        let sent = as_client_text(&table)
            .replace("Shield", "Round shield")
            .replace("Bouclier", "Bouclier rond");
        assert_ne!(sent, as_client_text(&table));
        let dry = env
            .store
            .write_with_schema(
                &env.schema,
                FILE,
                sent.as_bytes(),
                WriteOptions {
                    dry_run: true,
                    confirm: false,
                },
            )
            .unwrap();
        assert_eq!(dry.outcome.diff.modified.len(), 1, "{encoding:?}");
        assert!(!dry.outcome.diff.format_changed, "le format ne change pas");
        assert_eq!(dry.outcome.encoding, encoding);
        assert_eq!(env.file(), bytes, "dry-run: rien d'écrit");
        let real = env
            .store
            .write_with_schema(
                &env.schema,
                FILE,
                sent.as_bytes(),
                WriteOptions {
                    dry_run: false,
                    confirm: true,
                },
            )
            .unwrap();
        assert!(real.outcome.applied);
        // même diff en dry-run et en écriture réelle
        assert_eq!(dry.outcome.diff, real.outcome.diff);
        let written = env.file();
        let after = Table::parse(&written).unwrap();
        assert_eq!((after.encoding, after.newline), (encoding, newline));
        assert_eq!(
            &written[..if encoding == Encoding::Utf16LeBom {
                2
            } else if encoding == Encoding::Utf8Bom {
                3
            } else {
                0
            }],
            &bytes[..if encoding == Encoding::Utf16LeBom {
                2
            } else if encoding == Encoding::Utf8Bom {
                3
            } else {
                0
            }]
        );
        // seule la cellule modifiée diffère; la sauvegarde est la copie exacte de l'original
        let backups = env.backups();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), bytes);
        // le rapport d'une relecture sans écart: lire puis réécrire ne change plus rien
        let again = env
            .store
            .write_with_schema(
                &env.schema,
                FILE,
                &written,
                WriteOptions {
                    dry_run: false,
                    confirm: true,
                },
            )
            .unwrap();
        assert!(again.outcome.unchanged);
    }
}

#[test]
fn a_new_file_keeps_the_current_behaviour_utf8_without_bom_lf() {
    let env = Env::with(ASCII);
    let sent = as_client_text(&Table::parse(UTF16).unwrap());
    let w = env
        .store
        .write_with_schema(
            &env.schema,
            "nouveau.csv",
            sent.as_bytes(),
            WriteOptions {
                dry_run: false,
                confirm: true,
            },
        )
        .unwrap();
    assert!(w.outcome.applied && !w.outcome.existed);
    assert_eq!(
        (w.outcome.encoding, w.outcome.newline),
        (Encoding::Utf8, Newline::Lf)
    );
    let bytes = fs::read(env.csv_root.join("nouveau.csv")).unwrap();
    assert_eq!(bytes, sent.as_bytes());
    assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]) && !bytes.starts_with(&[0xFF, 0xFE]));
    assert!(!bytes.contains(&b'\r'));
}

#[test]
fn add_update_delete_keep_the_format_and_add_then_delete_restores_the_file() {
    for (bytes, encoding, newline) in cases() {
        let env = Env::with(bytes);
        let add = RowOp::Add {
            key: "Lore_900".into(),
            values: vals(&[
                (
                    "Name",
                    "NSLOCTEXT(\"\", \"Lore_900_Name\", \"Ñandú, \"\"x\"\"\")",
                ),
                ("Text", "NSLOCTEXT(\"\", \"Lore_900_Text\", \"a, b\")"),
                ("Category", "Récit"),
            ]),
        };
        let dry = env.row_op(&add, true).unwrap();
        assert_eq!(
            (dry.outcome().encoding, dry.outcome().newline),
            (encoding, newline)
        );
        assert_eq!(env.file(), bytes);
        env.row_op(&add, false).unwrap();
        let after_add = env.file();
        assert_ne!(after_add, bytes);
        let t = Table::parse(&after_add).unwrap();
        assert_eq!((t.encoding, t.newline), (encoding, newline), "{encoding:?}");
        assert_eq!(t.rows.last().unwrap().cells[0].text, "Lore_900");

        env.row_op(
            &RowOp::Update {
                key: "lore_900".into(),
                values: vals(&[("Category", "Légende")]),
            },
            false,
        )
        .unwrap();
        let t = Table::parse(&env.file()).unwrap();
        assert_eq!((t.encoding, t.newline), (encoding, newline));
        assert_eq!(t.rows.last().unwrap().cells[3].text, "Légende");

        env.row_op(
            &RowOp::Delete {
                key: "Lore_900".into(),
            },
            false,
        )
        .unwrap();
        // ajout puis suppression: le fichier est redevenu l'original, à l'octet près
        assert_eq!(env.file(), bytes, "{encoding:?}");
        assert_eq!(env.backups().len(), 3);
    }
}

#[test]
fn an_update_without_value_change_writes_nothing_in_utf16() {
    let env = Env::with(UTF16);
    let r = env
        .row_op(
            &RowOp::Update {
                key: "Lore_002".into(),
                values: vals(&[("Category", "Légende")]),
            },
            false,
        )
        .unwrap();
    assert!(r.no_change);
    assert_eq!(env.file(), UTF16);
    assert!(env.backups().is_empty());
}

#[test]
fn a_truncated_utf16_file_is_an_error_naming_the_encoding_and_nothing_is_modified() {
    let truncated = &UTF16[..UTF16.len() - 1];
    assert_eq!(truncated.len() % 2, 1);
    let err = Table::parse(truncated).unwrap_err();
    assert!(
        matches!(
            err,
            CsvError::InvalidEncoding {
                encoding: "UTF-16 LE avec BOM",
                ..
            }
        ),
        "{err}"
    );
    let message = err.to_string();
    assert!(
        message.contains("UTF-16 LE avec BOM") && message.contains("impaire"),
        "{message}"
    );

    let env = Env::with(truncated);
    // opération par ligne: refus net, fichier et sauvegardes intacts
    let e = env
        .row_op(
            &RowOp::Delete {
                key: "Lore_001".into(),
            },
            false,
        )
        .unwrap_err();
    assert!(e.to_string().contains("UTF-16 LE avec BOM"), "{e}");
    // écriture d'un contenu valide par-dessus: le fichier existant illisible bloque, comme avant
    let sent = as_client_text(&Table::parse(UTF16).unwrap());
    let e = env
        .store
        .write_with_schema(
            &env.schema,
            FILE,
            sent.as_bytes(),
            WriteOptions {
                dry_run: false,
                confirm: true,
            },
        )
        .unwrap_err();
    assert!(matches!(e, StoreError::ExistingUnreadable { .. }), "{e}");
    assert_eq!(env.file(), truncated);
    assert!(env.backups().is_empty());
}
