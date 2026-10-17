//! Lecture des DataTables Unreal au format CSV.
//!
//! Le lecteur est volontairement maison : il ne normalise rien. Il conserve ce qu'il
//! faut pour réécrire le fichier à l'octet près (BOM, type de fin de ligne, retour
//! final, quoting de chaque cellule), au lieu de le deviner à l'écriture.
//!
//! Format accepté : UTF-8, séparateur `,`, cellules quotées (`"..."`, guillemet
//! échappé par `""`, virgules et sauts de ligne admis entre guillemets) ou non quotées
//! (sans `"` ni saut de ligne). Fins de ligne `\n` ou `\r\n`, jamais mélangées.
//! Tout le reste est une erreur, jamais une correction silencieuse.

use crate::schema::Schema;
use crate::validate::{validate_row, ValidationError};
use crate::value::{RawRow, Row};

const BOM: char = '\u{feff}';

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CsvError {
    #[error("CSV: UTF-8 invalide à l'octet {offset}")]
    InvalidUtf8 { offset: usize },
    #[error("CSV: fichier vide")]
    Empty,
    #[error("CSV ligne {line}: guillemet fermant manquant (cellule quotée jamais terminée)")]
    UnterminatedQuote { line: usize },
    #[error("CSV ligne {line}: guillemet dans une cellule non quotée (reçu {found:?}; une cellule contenant `\"` doit être entièrement quotée)")]
    QuoteInUnquoted { line: usize, found: String },
    #[error("CSV ligne {line}: caractère {found:?} après un guillemet fermant (attendu `,` ou fin de ligne)")]
    TextAfterQuote { line: usize, found: char },
    #[error("CSV ligne {line}: retour chariot isolé (attendu `\\n` ou `\\r\\n`)")]
    LoneCarriageReturn { line: usize },
    #[error("CSV ligne {line}: fins de ligne mélangées (fichier {first}, cette ligne {found})")]
    MixedLineEndings {
        line: usize,
        first: &'static str,
        found: &'static str,
    },
    #[error("CSV ligne {line}: {found} cellule(s), l'en-tête en compte {expected}")]
    WrongColumnCount {
        line: usize,
        expected: usize,
        found: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Newline {
    Lf,
    CrLf,
}

impl Newline {
    pub fn as_str(self) -> &'static str {
        match self {
            Newline::Lf => "\n",
            Newline::CrLf => "\r\n",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Newline::Lf => "LF",
            Newline::CrLf => "CRLF",
        }
    }
}

/// Une cellule : son contenu décodé et son quoting d'origine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvCell {
    /// Contenu sans guillemets d'encadrement, `""` ramené à `"`.
    pub text: String,
    /// La cellule était encadrée de guillemets (`"",` ≠ `,` : vide quoté ≠ vide).
    pub quoted: bool,
}

/// Une ligne du fichier (en-tête ou donnée).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvRecord {
    /// Numéro de la ligne physique où commence l'enregistrement (1 = en-tête).
    pub line: usize,
    pub cells: Vec<CsvCell>,
}

/// Une DataTable lue, avec tout ce qu'il faut pour la réécrire à l'identique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub bom: bool,
    pub newline: Newline,
    /// Le fichier se termine par une fin de ligne.
    pub final_newline: bool,
    /// En-tête ; la première cellule est la colonne clé (`---` dans les exports observés).
    pub header: CsvRecord,
    /// Lignes de données ; la première cellule de chacune est la clé de ligne.
    pub rows: Vec<CsvRecord>,
}

impl Table {
    pub fn parse(bytes: &[u8]) -> Result<Table, CsvError> {
        let text = std::str::from_utf8(bytes).map_err(|e| CsvError::InvalidUtf8 {
            offset: e.valid_up_to(),
        })?;
        let (bom, text) = match text.strip_prefix(BOM) {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        if text.is_empty() {
            return Err(CsvError::Empty);
        }
        Parser::new(text).run(bom)
    }

    /// Les lignes de données sous forme de `RawRow`, prêtes pour `validate_row`.
    /// Les noms de colonnes viennent de l'en-tête (hors colonne clé).
    pub fn raw_rows(&self) -> Vec<RawRow> {
        self.rows
            .iter()
            .map(|rec| RawRow {
                line: rec.line,
                key: rec.cells[0].text.clone(),
                cells: self
                    .header
                    .cells
                    .iter()
                    .zip(&rec.cells)
                    .skip(1)
                    .map(|(h, c)| (h.text.clone(), c.text.clone()))
                    .collect(),
            })
            .collect()
    }
}

/// Valide toute la table : l'en-tête d'abord (colonnes inconnues, colonnes requises
/// absentes), puis chaque ligne. Renvoie toutes les erreurs trouvées.
pub fn validate_table(schema: &Schema, table: &Table) -> Result<Vec<Row>, Vec<ValidationError>> {
    let mut errors = Vec::new();
    let columns = &table.header.cells[1..];
    let line = table.header.line;
    for col in columns {
        if schema.field(&col.text).is_none() {
            errors.push(ValidationError {
                line,
                column: Some(col.text.clone()),
                received: Some(col.text.clone()),
                expected: format!(
                    "une colonne du schéma '{}': {}",
                    schema.name,
                    schema
                        .fields
                        .iter()
                        .map(|f| f.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
    }
    for field in schema.fields.iter().filter(|f| f.required) {
        if !columns.iter().any(|c| c.text == field.name) {
            errors.push(ValidationError {
                line,
                column: Some(field.name.clone()),
                received: None,
                expected: "cette colonne dans l'en-tête (champ requis)".into(),
            });
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut rows = Vec::with_capacity(table.rows.len());
    for raw in table.raw_rows() {
        match validate_row(schema, &raw) {
            Ok(row) => rows.push(row),
            Err(mut e) => errors.append(&mut e),
        }
    }
    if errors.is_empty() {
        Ok(rows)
    } else {
        Err(errors)
    }
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    line: usize,
    newline: Option<Newline>,
}

enum End {
    Comma,
    Newline,
    Eof,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Parser {
            chars: text.chars().peekable(),
            line: 1,
            newline: None,
        }
    }

    fn run(mut self, bom: bool) -> Result<Table, CsvError> {
        let mut records: Vec<CsvRecord> = Vec::new();
        let mut final_newline: bool;
        loop {
            let start = self.line;
            let mut cells = Vec::new();
            loop {
                cells.push(self.cell()?);
                match self.terminator()? {
                    End::Comma => continue,
                    End::Newline => {
                        final_newline = self.chars.peek().is_none();
                        break;
                    }
                    End::Eof => {
                        final_newline = false;
                        break;
                    }
                }
            }
            records.push(CsvRecord { line: start, cells });
            if self.chars.peek().is_none() {
                break;
            }
        }

        let mut iter = records.into_iter();
        let header = iter.next().ok_or(CsvError::Empty)?;
        let rows: Vec<CsvRecord> = iter.collect();
        for rec in &rows {
            if rec.cells.len() != header.cells.len() {
                return Err(CsvError::WrongColumnCount {
                    line: rec.line,
                    expected: header.cells.len(),
                    found: rec.cells.len(),
                });
            }
        }
        Ok(Table {
            bom,
            newline: self.newline.unwrap_or(Newline::Lf),
            final_newline,
            header,
            rows,
        })
    }

    fn cell(&mut self) -> Result<CsvCell, CsvError> {
        if self.chars.peek() == Some(&'"') {
            self.chars.next();
            let mut text = String::new();
            loop {
                match self.chars.next() {
                    None => return Err(CsvError::UnterminatedQuote { line: self.line }),
                    Some('"') => {
                        if self.chars.peek() == Some(&'"') {
                            self.chars.next();
                            text.push('"');
                        } else {
                            return Ok(CsvCell { text, quoted: true });
                        }
                    }
                    Some(c) => {
                        if c == '\n' {
                            self.line += 1;
                        }
                        text.push(c);
                    }
                }
            }
        }
        let mut text = String::new();
        while let Some(&c) = self.chars.peek() {
            match c {
                ',' | '\n' | '\r' => break,
                '"' => {
                    text.push(c);
                    return Err(CsvError::QuoteInUnquoted {
                        line: self.line,
                        found: text,
                    });
                }
                _ => {
                    text.push(c);
                    self.chars.next();
                }
            }
        }
        Ok(CsvCell {
            text,
            quoted: false,
        })
    }

    /// Consomme ce qui suit une cellule : `,`, fin de ligne ou fin de fichier.
    fn terminator(&mut self) -> Result<End, CsvError> {
        match self.chars.next() {
            None => Ok(End::Eof),
            Some(',') => Ok(End::Comma),
            Some('\n') => {
                self.note_newline(Newline::Lf)?;
                self.line += 1;
                Ok(End::Newline)
            }
            Some('\r') => {
                if self.chars.next() != Some('\n') {
                    return Err(CsvError::LoneCarriageReturn { line: self.line });
                }
                self.note_newline(Newline::CrLf)?;
                self.line += 1;
                Ok(End::Newline)
            }
            Some(found) => Err(CsvError::TextAfterQuote {
                line: self.line,
                found,
            }),
        }
    }

    fn note_newline(&mut self, found: Newline) -> Result<(), CsvError> {
        match self.newline {
            None => {
                self.newline = Some(found);
                Ok(())
            }
            Some(first) if first == found => Ok(()),
            Some(first) => Err(CsvError::MixedLineEndings {
                line: self.line,
                first: first.label(),
                found: found.label(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;
    use std::path::Path;

    fn parse(s: &str) -> Result<Table, CsvError> {
        Table::parse(s.as_bytes())
    }

    fn texts(rec: &CsvRecord) -> Vec<&str> {
        rec.cells.iter().map(|c| c.text.as_str()).collect()
    }

    fn fixture() -> Vec<u8> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/DT_LightningRodLoot_Common.csv"
        );
        std::fs::read(path).unwrap()
    }

    #[test]
    fn reads_the_real_devkit_fixture() {
        let t = Table::parse(&fixture()).unwrap();
        assert!(!t.bom);
        assert_eq!(t.newline, Newline::Lf);
        assert!(t.final_newline);
        assert_eq!(texts(&t.header), ["---", "ItemTemplateID", "MinAmount", "MaxAmount"]);
        assert!(t.header.cells.iter().all(|c| !c.quoted));
        assert_eq!(t.rows.len(), 4);
        assert_eq!(texts(&t.rows[0]), ["1", "11011", "5", "15"]);
        assert_eq!(texts(&t.rows[3]), ["4", "10005", "5", "15"]);
        for r in &t.rows {
            assert!(!r.cells[0].quoted, "la clé n'est pas quotée");
            assert!(r.cells[1..].iter().all(|c| c.quoted), "les valeurs sont quotées");
        }
        assert_eq!(t.rows[0].line, 2);
        assert_eq!(t.rows[3].line, 5);
    }

    #[test]
    fn fixture_validates_against_its_schema() {
        let schema_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/lightning_rod_loot.toml"
        );
        let schema = Schema::from_file(Path::new(schema_path)).unwrap();
        let t = Table::parse(&fixture()).unwrap();
        let rows = validate_table(&schema, &t).unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].key, "1");
        let c = rows[0].cell("ItemTemplateID").unwrap();
        assert_eq!((c.raw.as_str(), &c.value), ("11011", &Value::Int(11011)));
    }

    #[test]
    fn header_errors_are_reported_once() {
        let schema = Schema::from_toml_str(
            "name='t'\n[[fields]]\nname='A'\ntype='int'\nrequired=true\n",
        )
        .unwrap();
        let t = parse("---,B\n1,2\n2,3\n").unwrap();
        let e = validate_table(&schema, &t).unwrap_err();
        assert_eq!(e.len(), 2); // B inconnue, A absente
        assert!(e.iter().all(|x| x.line == 1));
    }

    #[test]
    fn row_errors_carry_the_physical_line() {
        let schema = Schema::from_toml_str("name='t'\n[[fields]]\nname='A'\ntype='int'\n").unwrap();
        let t = parse("---,A\n1,5\n2,x\n").unwrap();
        let e = validate_table(&schema, &t).unwrap_err();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].line, 3);
    }

    // Les cas limites ci-dessous suivent la norme CSV (RFC 4180) :
    // non validé sur un export Devkit réel.

    #[test]
    fn escaped_quote_and_comma_inside_quotes() {
        // non validé sur un export Devkit réel
        let t = parse("---,A\n1,\"a \"\"b\"\", c\"\n").unwrap();
        let c = &t.rows[0].cells[1];
        assert_eq!(c.text, "a \"b\", c");
        assert!(c.quoted);
    }

    #[test]
    fn newline_inside_quotes_counts_lines() {
        // non validé sur un export Devkit réel
        let t = parse("---,A\n1,\"x\ny\"\n2,z\n").unwrap();
        assert_eq!(t.rows[0].cells[1].text, "x\ny");
        assert_eq!(t.rows[1].line, 4);
    }

    #[test]
    fn crlf_detected() {
        // non validé sur un export Devkit réel
        let t = parse("---,A\r\n1,2\r\n").unwrap();
        assert_eq!(t.newline, Newline::CrLf);
        assert!(t.final_newline);
        assert_eq!(texts(&t.rows[0]), ["1", "2"]);
    }

    #[test]
    fn mixed_line_endings_rejected() {
        // non validé sur un export Devkit réel
        let e = parse("---,A\r\n1,2\n").unwrap_err();
        assert!(matches!(e, CsvError::MixedLineEndings { line: 2, .. }));
        assert!(matches!(
            parse("---,A\r1,2\n").unwrap_err(),
            CsvError::LoneCarriageReturn { line: 1 }
        ));
    }

    #[test]
    fn bom_detected_and_not_part_of_the_header() {
        // non validé sur un export Devkit réel
        let t = Table::parse(b"\xef\xbb\xbf---,A\n1,2\n").unwrap();
        assert!(t.bom);
        assert_eq!(t.header.cells[0].text, "---");
    }

    #[test]
    fn empty_cell_quoted_vs_unquoted() {
        // non validé sur un export Devkit réel
        let t = parse("---,A,B\n1,\"\",\n").unwrap();
        assert_eq!(
            t.rows[0].cells[1],
            CsvCell { text: String::new(), quoted: true }
        );
        assert_eq!(
            t.rows[0].cells[2],
            CsvCell { text: String::new(), quoted: false }
        );
    }

    #[test]
    fn no_final_newline() {
        // non validé sur un export Devkit réel
        let t = parse("---,A\n1,2").unwrap();
        assert!(!t.final_newline);
        assert_eq!(t.rows.len(), 1);
    }

    #[test]
    fn accents_are_kept() {
        // non validé sur un export Devkit réel
        let t = parse("---,A\n1,\"épée ⚔\"\n").unwrap();
        assert_eq!(t.rows[0].cells[1].text, "épée ⚔");
    }

    #[test]
    fn malformed_files_are_errors() {
        assert_eq!(parse("").unwrap_err(), CsvError::Empty);
        assert_eq!(Table::parse(b"\xef\xbb\xbf").unwrap_err(), CsvError::Empty);
        assert!(matches!(
            Table::parse(b"---,A\n1,\xff\n").unwrap_err(),
            CsvError::InvalidUtf8 { offset: 8 }
        ));
        assert!(matches!(
            parse("---,A\n1,\"abc\n").unwrap_err(),
            CsvError::UnterminatedQuote { .. }
        ));
        assert!(matches!(
            parse("---,A\n1,ab\"c\n").unwrap_err(),
            CsvError::QuoteInUnquoted { line: 2, .. }
        ));
        assert!(matches!(
            parse("---,A\n1,\"a\"b\n").unwrap_err(),
            CsvError::TextAfterQuote { line: 2, found: 'b' }
        ));
        assert!(matches!(
            parse("---,A,B\n1,2\n").unwrap_err(),
            CsvError::WrongColumnCount { line: 2, expected: 3, found: 2 }
        ));
        // ligne vide au milieu : une seule cellule vide, donc mauvais nombre de colonnes
        assert!(matches!(
            parse("---,A\n\n1,2\n").unwrap_err(),
            CsvError::WrongColumnCount { line: 2, .. }
        ));
    }
}
