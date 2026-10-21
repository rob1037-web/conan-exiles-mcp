//! Lecture des DataTables Unreal au format CSV.
//!
//! Le lecteur est volontairement maison : il ne normalise rien. Il conserve ce qu'il
//! faut pour réécrire le fichier à l'octet près (BOM, type de fin de ligne, retour
//! final, quoting de chaque cellule), au lieu de le deviner à l'écriture.
//!
//! Encodage détecté par le BOM : `FF FE` = UTF-16 LE (export Devkit dès qu'un texte n'est pas ASCII),
//! `EF BB BF` = UTF-8 avec BOM, sinon UTF-8 strict (les exports ASCII). Le BOM est retiré avant le
//! parsing et l'encodage est conservé dans `Table::encoding` pour la réécriture. UTF-16 BE n'est pas
//! pris en charge (erreur explicite).
//!
//! Format accepté : texte décodé, séparateur `,`, cellules quotées (`"..."`, guillemet
//! échappé par `""`, virgules et sauts de ligne admis entre guillemets) ou non quotées
//! (sans `"` ni saut de ligne). Fins de ligne `\n` ou `\r\n`, jamais mélangées.
//! Tout le reste est une erreur, jamais une correction silencieuse.

use crate::schema::Schema;
use crate::validate::{validate_row, Rule, ValidationError};
use crate::value::{RawRow, Row};

const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
const UTF16_LE_BOM: [u8; 2] = [0xFF, 0xFE];
const UTF16_BE_BOM: [u8; 2] = [0xFE, 0xFF];

/// Encodage d'un fichier CSV, avec son BOM. Détecté à la lecture, restitué tel quel à la réécriture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// UTF-8 sans BOM (les exports ASCII du Devkit en font partie).
    Utf8,
    /// UTF-8 avec BOM (`EF BB BF`).
    Utf8Bom,
    /// UTF-16 petit-boutiste avec BOM (`FF FE`), l'export du Devkit dès qu'un texte n'est pas ASCII.
    Utf16LeBom,
}

impl Encoding {
    /// Identifiant stable, pour les réponses des outils.
    pub fn id(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf8Bom => "utf-8-bom",
            Encoding::Utf16LeBom => "utf-16le-bom",
        }
    }

    /// Libellé lisible, pour les messages.
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8 sans BOM",
            Encoding::Utf8Bom => "UTF-8 avec BOM",
            Encoding::Utf16LeBom => "UTF-16 LE avec BOM",
        }
    }

    pub fn has_bom(self) -> bool {
        !matches!(self, Encoding::Utf8)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CsvError {
    #[error("CSV: encodage détecté {encoding}, mais {detail} (à l'octet {offset})")]
    InvalidEncoding {
        encoding: &'static str,
        offset: usize,
        detail: String,
    },
    #[error("CSV: encodage {encoding} détecté (BOM {bom}), non pris en charge: seuls UTF-8 (avec ou sans BOM) et UTF-16 LE avec BOM sont lus")]
    UnsupportedEncoding {
        encoding: &'static str,
        bom: &'static str,
    },
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
    /// Encodage du fichier, BOM compris (UTF-8, UTF-8 avec BOM, UTF-16 LE avec BOM).
    pub encoding: Encoding,
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
        let (encoding, text) = decode(bytes)?;
        if text.is_empty() {
            return Err(CsvError::Empty);
        }
        Parser::new(&text).run(encoding)
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

/// Détecte l'encodage par le BOM et décode le texte (BOM retiré). Sans BOM: UTF-8 strict.
fn decode(bytes: &[u8]) -> Result<(Encoding, String), CsvError> {
    if let Some(body) = bytes.strip_prefix(&UTF16_LE_BOM) {
        let encoding = Encoding::Utf16LeBom;
        if body.len() % 2 != 0 {
            return Err(CsvError::InvalidEncoding {
                encoding: encoding.label(),
                offset: bytes.len() - 1,
                detail: format!(
                    "la longueur est impaire ({} octets; un fichier UTF-16 a un nombre pair d'octets): \
                     fichier tronqué ou corrompu",
                    bytes.len()
                ),
            });
        }
        let mut text = String::with_capacity(body.len() / 2);
        let units = body
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]));
        let mut index = 0usize;
        for decoded in char::decode_utf16(units) {
            match decoded {
                Ok(c) => {
                    index += c.len_utf16();
                    text.push(c);
                }
                Err(e) => {
                    return Err(CsvError::InvalidEncoding {
                        encoding: encoding.label(),
                        offset: 2 + 2 * index,
                        detail: format!(
                            "unité UTF-16 isolée 0x{:04X} (paire de substitution incomplète, fichier tronqué ou corrompu)",
                            e.unpaired_surrogate()
                        ),
                    })
                }
            }
        }
        return Ok((encoding, text));
    }
    if bytes.starts_with(&UTF16_BE_BOM) {
        return Err(CsvError::UnsupportedEncoding {
            encoding: "UTF-16 BE avec BOM",
            bom: "FE FF",
        });
    }
    let (encoding, body, skipped) = match bytes.strip_prefix(&UTF8_BOM) {
        Some(rest) => (Encoding::Utf8Bom, rest, UTF8_BOM.len()),
        None => (Encoding::Utf8, bytes, 0),
    };
    match std::str::from_utf8(body) {
        Ok(text) => Ok((encoding, text.to_owned())),
        Err(e) => Err(CsvError::InvalidEncoding {
            encoding: encoding.label(),
            offset: skipped + e.valid_up_to(),
            detail: "le fichier n'est pas de l'UTF-8 valide (aucun BOM UTF-16 détecté)".into(),
        }),
    }
}

/// Erreur d'ajout de ligne.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    #[error("ligne '{key}': colonne '{column}' absente de l'en-tête (colonnes: {header})")]
    UnknownColumn {
        key: String,
        column: String,
        header: String,
    },
    #[error("ligne '{key}': colonne '{column}' absente de la ligne (colonnes de l'en-tête: {header}); aucune colonne n'est complétée automatiquement")]
    MissingColumn {
        key: String,
        column: String,
        header: String,
    },
    #[error("ligne '{key}': clé déjà présente dans la table (ligne {existing_line}, clé '{existing}'; comparaison insensible à la casse ASCII)")]
    DuplicateKey {
        key: String,
        existing: String,
        existing_line: usize,
    },
}

impl Table {
    /// Sérialise la table en mémoire, sans aucun accès disque, dans son encodage (BOM compris). Chaque cellule est
    /// réécrite avec son quoting d'origine ; une cellule non quotée dont le texte
    /// exigerait des guillemets (cas impossible après `parse`) est quotée d'office.
    pub fn to_bytes(&self) -> Vec<u8> {
        let nl = self.newline.as_str();
        let mut out = String::new();
        let mut records = std::iter::once(&self.header).chain(&self.rows).peekable();
        while let Some(rec) = records.next() {
            for (i, cell) in rec.cells.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_cell(&mut out, cell);
            }
            if records.peek().is_some() || self.final_newline {
                out.push_str(nl);
            }
        }
        match self.encoding {
            Encoding::Utf8 => out.into_bytes(),
            Encoding::Utf8Bom => [&UTF8_BOM[..], out.as_bytes()].concat(),
            Encoding::Utf16LeBom => {
                let mut bytes = UTF16_LE_BOM.to_vec();
                bytes.extend(out.encode_utf16().flat_map(u16::to_le_bytes));
                bytes
            }
        }
    }

    /// Ajoute une ligne validée. Convention des nouvelles lignes, observée dans les
    /// exports Devkit : clé non quotée, valeurs quotées. Refuse, sans rien modifier :
    /// une colonne inconnue de l'en-tête, une colonne de l'en-tête absente de la ligne
    /// (jamais complétée en silence) et une clé déjà présente (casse ASCII ignorée,
    /// comme les FName).
    pub fn push_row(&mut self, row: &Row) -> Result<(), WriteError> {
        let columns = &self.header.cells[1..];
        let header_list = || {
            columns
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        if let Some(bad) = row
            .cells
            .iter()
            .find(|c| !columns.iter().any(|h| h.text == c.column))
        {
            return Err(WriteError::UnknownColumn {
                key: row.key.clone(),
                column: bad.column.clone(),
                header: header_list(),
            });
        }
        if let Some(h) = columns
            .iter()
            .find(|h| !row.cells.iter().any(|c| c.column == h.text))
        {
            return Err(WriteError::MissingColumn {
                key: row.key.clone(),
                column: h.text.clone(),
                header: header_list(),
            });
        }
        if let Some(existing) = self
            .rows
            .iter()
            .find(|r| r.cells[0].text.eq_ignore_ascii_case(&row.key))
        {
            return Err(WriteError::DuplicateKey {
                key: row.key.clone(),
                existing: existing.cells[0].text.clone(),
                existing_line: existing.line,
            });
        }
        let mut cells = Vec::with_capacity(self.header.cells.len());
        cells.push(CsvCell {
            text: row.key.clone(),
            quoted: false,
        });
        for h in columns {
            let text = row
                .cells
                .iter()
                .find(|c| c.column == h.text)
                .map(|c| c.raw.clone())
                .ok_or_else(|| WriteError::MissingColumn {
                    key: row.key.clone(),
                    column: h.text.clone(),
                    header: header_list(),
                })?;
            cells.push(CsvCell { text, quoted: true });
        }
        let last = self.rows.last().unwrap_or(&self.header);
        let line = last.line
            + 1
            + last
                .cells
                .iter()
                .map(|c| c.text.matches('\n').count())
                .sum::<usize>();
        self.rows.push(CsvRecord { line, cells });
        Ok(())
    }
}

fn write_cell(out: &mut String, cell: &CsvCell) {
    let needs_quotes = cell.text.contains([',', '"', '\n', '\r']);
    if cell.quoted || needs_quotes {
        out.push('"');
        out.push_str(&cell.text.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(&cell.text);
    }
}

/// Valide toute la table contre le schéma et renvoie **toutes** les erreurs trouvées.
///
/// 1. L'en-tête doit correspondre exactement au schéma: colonne clé (`key_column`), puis les colonnes du schéma, mêmes noms, même ordre, sans doublon. Tout écart
///    est une dérive de schéma (`Rule::is_schema_drift`); les lignes ne sont alors pas validées.
/// 2. Chaque ligne: types, bornes, champs obligatoires, valeurs d'enum.
/// 3. Les clés de ligne sont uniques (casse ASCII ignorée, comme les FName).
pub fn validate_table(schema: &Schema, table: &Table) -> Result<Vec<Row>, Vec<ValidationError>> {
    let mut errors = validate_header(schema, table);
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
    let mut first_seen: std::collections::HashMap<String, (usize, String)> =
        std::collections::HashMap::new();
    for rec in &table.rows {
        let key = &rec.cells[0].text;
        if key.trim().is_empty() {
            continue; // déjà signalée par la validation de ligne
        }
        match first_seen.get(&key.to_ascii_lowercase()) {
            Some((line, original)) => errors.push(ValidationError {
                line: rec.line,
                key: Some(key.clone()),
                column: None,
                received: Some(key.clone()),
                rule: Rule::DuplicateKey,
                expected: format!(
                    "une clé unique (déjà utilisée ligne {line} par '{original}'; casse ASCII ignorée)"
                ),
            }),
            None => {
                first_seen.insert(key.to_ascii_lowercase(), (rec.line, key.clone()));
            }
        }
    }
    errors.sort_by_key(|e| e.line);
    if errors.is_empty() {
        Ok(rows)
    } else {
        Err(errors)
    }
}

fn validate_header(schema: &Schema, table: &Table) -> Vec<ValidationError> {
    let line = table.header.line;
    let mut errors = Vec::new();
    let mut drift = |rule, column: Option<&str>, received: Option<&str>, expected: String| {
        errors.push(ValidationError {
            line,
            key: None,
            column: column.map(str::to_owned),
            received: received.map(str::to_owned),
            rule,
            expected,
        });
    };
    {
        let expected_key = &schema.key_column;
        let got = &table.header.cells[0].text;
        if got != expected_key {
            drift(
                Rule::KeyHeaderMismatch,
                None,
                Some(got),
                format!("{expected_key:?} comme en-tête de la colonne clé (key_column)"),
            );
        }
    }
    let columns: Vec<&str> = table.header.cells[1..]
        .iter()
        .map(|c| c.text.as_str())
        .collect();
    let expected: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
    let mut structural = false;
    for (i, col) in columns.iter().enumerate() {
        if columns[..i].contains(col) {
            structural = true;
            drift(
                Rule::DuplicateColumn,
                Some(col),
                Some(col),
                "une seule colonne de ce nom dans l'en-tête".into(),
            );
        } else if !expected.contains(col) {
            structural = true;
            drift(
                Rule::UnknownColumn,
                Some(col),
                Some(col),
                format!(
                    "une colonne du schéma '{}': {} (dérive de schéma: l'export contient une colonne inconnue)",
                    schema.name,
                    expected.join(", ")
                ),
            );
        }
    }
    // Le Devkit exporte tous les champs de la struct: une colonne du schéma absente de
    // l'en-tête (requise ou non) signifie que le schéma ne correspond pas à ce fichier.
    for name in &expected {
        if !columns.contains(name) {
            structural = true;
            drift(
                Rule::MissingColumn,
                Some(name),
                None,
                "cette colonne dans l'en-tête (le Devkit exporte tous les champs: dérive de schéma)".into(),
            );
        }
    }
    if !structural && columns != expected {
        drift(
            Rule::ColumnOrder,
            None,
            Some(&columns.join(", ")),
            format!(
                "les colonnes dans l'ordre du schéma: {}",
                expected.join(", ")
            ),
        );
    }
    errors
}

/// Écrit un flottant comme le Devkit l'exporte (six décimales: `10` -> `10.000000`), seulement
/// si la valeur reste exactement la même ; sinon (plus de six décimales significatives) le texte
/// d'origine est conservé tel quel, jamais arrondi.
fn six_decimals(text: &str) -> String {
    let Ok(value) = text.parse::<f64>() else {
        return text.to_owned();
    };
    let formatted = format!("{value:.6}");
    match formatted.parse::<f64>() {
        Ok(back) if value.is_finite() && back == value => formatted,
        _ => text.to_owned(),
    }
}

impl Table {
    /// Met la table au format des exports du Devkit, quel que soit le format reçu:
    /// en-tête et clés de ligne sans guillemets, valeurs entre guillemets, flottants à six
    /// décimales (champs `float` du schéma, sans jamais arrondir), fins de ligne LF, UTF-8 sans BOM,
    /// avec retour final. L'appelant peut ensuite restituer l'encodage et les fins de ligne d'un
    /// fichier existant (`CsvStore::write_with_schema` le fait). Les valeurs ne changent pas (hors écriture des flottants).
    /// Tolérant: une table invalide est mise en forme au mieux, sans erreur.
    pub fn to_devkit_format(&self, schema: &Schema) -> Table {
        let is_float: Vec<bool> = self
            .header
            .cells
            .iter()
            .enumerate()
            .map(|(i, h)| {
                i > 0
                    && schema
                        .field(&h.text)
                        .is_some_and(|f| matches!(f.ty, crate::schema::FieldType::Float { .. }))
            })
            .collect();
        let plain = |c: &CsvCell| CsvCell {
            text: c.text.clone(),
            quoted: false,
        };
        Table {
            encoding: Encoding::Utf8,
            newline: Newline::Lf,
            final_newline: true,
            header: CsvRecord {
                line: self.header.line,
                cells: self.header.cells.iter().map(plain).collect(),
            },
            rows: self
                .rows
                .iter()
                .map(|r| CsvRecord {
                    line: r.line,
                    cells: r
                        .cells
                        .iter()
                        .enumerate()
                        .map(|(i, c)| match i {
                            0 => plain(c),
                            _ if is_float.get(i) == Some(&true) && !c.text.is_empty() => CsvCell {
                                text: six_decimals(&c.text),
                                quoted: true,
                            },
                            _ => CsvCell {
                                text: c.text.clone(),
                                quoted: true,
                            },
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// Construit une table à partir de lignes de texte `[clé, valeur par colonne du schéma…]`,
    /// avec la convention des exports Devkit (en-tête et clés non quotés, valeurs quotées, LF,
    /// retour final). Un nombre de valeurs incorrect est signalé (`Rule::ColumnCount`) pour
    /// toutes les lignes concernées. Les numéros de ligne suivent ceux d'un CSV: 1 = en-tête.
    pub fn for_schema(
        schema: &Schema,
        rows: &[Vec<String>],
    ) -> Result<Table, Vec<ValidationError>> {
        let width = schema.fields.len() + 1;
        let errors: Vec<ValidationError> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.len() != width)
            .map(|(i, r)| ValidationError {
                line: i + 2,
                key: r.first().cloned(),
                column: None,
                received: Some(format!("{} valeur(s)", r.len())),
                rule: Rule::ColumnCount,
                expected: format!(
                    "{width} valeurs: la clé puis {}",
                    schema
                        .fields
                        .iter()
                        .map(|f| f.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })
            .collect();
        if !errors.is_empty() {
            return Err(errors);
        }
        let plain = |text: String| CsvCell {
            text,
            quoted: false,
        };
        let quoted = |text: String| CsvCell { text, quoted: true };
        let header_cells = std::iter::once(schema.key_column.clone())
            .chain(schema.fields.iter().map(|f| f.name.clone()))
            .map(plain)
            .collect();
        Ok(Table {
            encoding: Encoding::Utf8,
            newline: Newline::Lf,
            final_newline: true,
            header: CsvRecord {
                line: 1,
                cells: header_cells,
            },
            rows: rows
                .iter()
                .enumerate()
                .map(|(i, r)| CsvRecord {
                    line: i + 2,
                    cells: r
                        .iter()
                        .enumerate()
                        .map(|(c, v)| {
                            if c == 0 {
                                plain(v.clone())
                            } else {
                                quoted(v.clone())
                            }
                        })
                        .collect(),
                })
                .collect(),
        })
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

    fn run(mut self, encoding: Encoding) -> Result<Table, CsvError> {
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
            encoding,
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
    use crate::value::{Cell, Value};

    fn parse(s: &str) -> Result<Table, CsvError> {
        Table::parse(s.as_bytes())
    }

    fn texts(rec: &CsvRecord) -> Vec<&str> {
        rec.cells.iter().map(|c| c.text.as_str()).collect()
    }

    // Intégrés à la compilation : les tests cross-compilés tournent sous Windows,
    // où le chemin du dépôt WSL n'existe pas.
    fn fixture() -> Vec<u8> {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/DT_LightningRodLoot_Common.csv"
        ))
        .to_vec()
    }

    fn lightning_schema() -> Schema {
        Schema::from_toml_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/lightning_rod_loot.toml"
        )))
        .unwrap()
    }

    #[test]
    fn reads_the_real_devkit_fixture() {
        let t = Table::parse(&fixture()).unwrap();
        assert_eq!(t.encoding, Encoding::Utf8);
        assert_eq!(t.newline, Newline::Lf);
        assert!(t.final_newline);
        assert_eq!(
            texts(&t.header),
            ["---", "ItemTemplateID", "MinAmount", "MaxAmount"]
        );
        assert!(t.header.cells.iter().all(|c| !c.quoted));
        assert_eq!(t.rows.len(), 4);
        assert_eq!(texts(&t.rows[0]), ["1", "11011", "5", "15"]);
        assert_eq!(texts(&t.rows[3]), ["4", "10005", "5", "15"]);
        for r in &t.rows {
            assert!(!r.cells[0].quoted, "la clé n'est pas quotée");
            assert!(
                r.cells[1..].iter().all(|c| c.quoted),
                "les valeurs sont quotées"
            );
        }
        assert_eq!(t.rows[0].line, 2);
        assert_eq!(t.rows[3].line, 5);
    }

    #[test]
    fn fixture_validates_against_its_schema() {
        let schema = lightning_schema();
        let t = Table::parse(&fixture()).unwrap();
        let rows = validate_table(&schema, &t).unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].key, "1");
        let c = rows[0].cell("ItemTemplateID").unwrap();
        assert_eq!((c.raw.as_str(), &c.value), ("11011", &Value::Int(11011)));
    }

    #[test]
    fn optional_column_missing_from_header_is_an_error_too() {
        let schema = Schema::from_toml_str(
            "name='t'\nkey_column='---'\n[[fields]]\nname='A'\ntype='int'\n[[fields]]\nname='B'\ntype='int'\n",
        )
        .unwrap();
        let t = parse("---,A\n1,2\n").unwrap();
        let e = validate_table(&schema, &t).unwrap_err();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].column.as_deref(), Some("B"));
        assert_eq!(e[0].received, None);
    }

    #[test]
    fn header_errors_are_reported_once() {
        let schema = Schema::from_toml_str(
            "name='t'\nkey_column='---'\n[[fields]]\nname='A'\ntype='int'\nrequired=true\n",
        )
        .unwrap();
        let t = parse("---,B\n1,2\n2,3\n").unwrap();
        let e = validate_table(&schema, &t).unwrap_err();
        assert_eq!(e.len(), 2); // B inconnue, A absente
        assert!(e.iter().all(|x| x.line == 1));
    }

    #[test]
    fn row_errors_carry_the_physical_line() {
        let schema =
            Schema::from_toml_str("name='t'\nkey_column='---'\n[[fields]]\nname='A'\ntype='int'\n")
                .unwrap();
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
        assert_eq!(t.encoding, Encoding::Utf8Bom);
        assert_eq!(t.header.cells[0].text, "---");
    }

    #[test]
    fn empty_cell_quoted_vs_unquoted() {
        // non validé sur un export Devkit réel
        let t = parse("---,A,B\n1,\"\",\n").unwrap();
        assert_eq!(
            t.rows[0].cells[1],
            CsvCell {
                text: String::new(),
                quoted: true
            }
        );
        assert_eq!(
            t.rows[0].cells[2],
            CsvCell {
                text: String::new(),
                quoted: false
            }
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
            CsvError::InvalidEncoding {
                encoding: "UTF-8 sans BOM",
                offset: 8,
                ..
            }
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
            CsvError::TextAfterQuote {
                line: 2,
                found: 'b'
            }
        ));
        assert!(matches!(
            parse("---,A,B\n1,2\n").unwrap_err(),
            CsvError::WrongColumnCount {
                line: 2,
                expected: 3,
                found: 2
            }
        ));
        // ligne vide au milieu : une seule cellule vide, donc mauvais nombre de colonnes
        assert!(matches!(
            parse("---,A\n\n1,2\n").unwrap_err(),
            CsvError::WrongColumnCount { line: 2, .. }
        ));
    }

    // --- Écriture -----------------------------------------------------------------

    #[test]
    fn fixture_round_trips_byte_for_byte() {
        let original = fixture();
        let t = Table::parse(&original).unwrap();
        assert_eq!(t.to_bytes(), original);
    }

    #[test]
    fn edge_cases_round_trip_byte_for_byte() {
        // Cas limites selon la norme CSV : non validé sur un export Devkit réel
        let cases: &[&[u8]] = &[
            b"---,A\r\n1,\"x\"\r\n",
            b"\xef\xbb\xbf---,A\n1,\"x\"\n",
            b"---,A\n1,\"a \"\"b\"\", c\"\n",
            b"---,A\n1,\"x\ny\"\n",
            b"---,A,B\n1,\"\",\n",
            b"---,A\n1,\"\xc3\xa9p\xc3\xa9e\"",
            b"\"---\",\"A\"\n\"1\",2\n",
            b"---,A\n",
            b"---",
        ];
        for case in cases {
            let t = Table::parse(case).unwrap();
            assert_eq!(t.to_bytes(), *case, "{:?}", String::from_utf8_lossy(case));
        }
    }

    #[test]
    fn new_row_follows_observed_convention_and_rereads_valid() {
        let schema = lightning_schema();
        let original = fixture();
        let mut t = Table::parse(&original).unwrap();
        let raw = RawRow {
            line: 0,
            key: "5".into(),
            cells: vec![
                ("ItemTemplateID".into(), "12000".into()),
                ("MinAmount".into(), "1".into()),
                ("MaxAmount".into(), "3".into()),
            ],
        };
        let row = validate_row(&schema, &raw).unwrap();
        t.push_row(&row).unwrap();

        let mut expected = original.clone();
        expected.extend_from_slice(b"5,\"12000\",\"1\",\"3\"\n");
        let bytes = t.to_bytes();
        assert_eq!(bytes, expected);

        let again = Table::parse(&bytes).unwrap();
        assert_eq!(again.rows.len(), 5);
        assert_eq!(again.rows[4].line, 6);
        assert!(!again.rows[4].cells[0].quoted);
        assert!(again.rows[4].cells[1..].iter().all(|c| c.quoted));
        assert_eq!(validate_table(&schema, &again).unwrap().len(), 5);
        // les lignes d'origine sont intactes
        assert!(bytes.starts_with(&original));
    }

    fn two_col_schema() -> Schema {
        Schema::from_toml_str(
            "name='t'\nkey_column='---'\n[[fields]]\nname='A'\ntype='float'\n[[fields]]\nname='B'\ntype='int'\n",
        )
        .unwrap()
    }

    fn raw(key: &str, cells: &[(&str, &str)]) -> RawRow {
        RawRow {
            line: 0,
            key: key.into(),
            cells: cells
                .iter()
                .map(|(c, v)| ((*c).into(), (*v).into()))
                .collect(),
        }
    }

    #[test]
    fn pushed_row_keeps_raw_text_exactly() {
        let mut t = parse("---,A,B\n").unwrap();
        let row = validate_row(&two_col_schema(), &raw("k", &[("A", "1.50"), ("B", "")])).unwrap();
        t.push_row(&row).unwrap();
        assert_eq!(t.to_bytes(), b"---,A,B\nk,\"1.50\",\"\"\n");
    }

    #[test]
    fn validate_row_then_push_never_completes_a_missing_column() {
        // validate_row refuse déjà la colonne absente...
        let schema = two_col_schema();
        assert!(validate_row(&schema, &raw("k", &[("A", "1")])).is_err());
        // ... et push_row refuse une Row construite à la main qui l'omet.
        let mut t = parse("---,A,B\n").unwrap();
        let row = Row {
            line: 0,
            key: "k".into(),
            cells: vec![Cell {
                column: "A".into(),
                raw: "1".into(),
                value: Value::Raw,
            }],
        };
        let e = t.push_row(&row).unwrap_err();
        assert!(matches!(e, WriteError::MissingColumn { ref column, .. } if column == "B"));
        assert!(e.to_string().contains("colonnes de l'en-tête: A, B"));
        assert!(t.rows.is_empty());
    }

    #[test]
    fn push_row_rejects_duplicate_key_ignoring_ascii_case() {
        let schema = two_col_schema();
        let mut t = parse("---,A,B\nRow_A,\"1\",\"2\"\n").unwrap();
        let cells = [("A", "1"), ("B", "2")];
        for key in ["Row_A", "row_a", "ROW_A"] {
            let row = validate_row(&schema, &raw(key, &cells)).unwrap();
            let e = t.push_row(&row).unwrap_err();
            assert!(
                matches!(e, WriteError::DuplicateKey { existing_line: 2, ref existing, .. } if existing == "Row_A"),
                "{key}"
            );
        }
        assert_eq!(t.rows.len(), 1);
        let ok = validate_row(&schema, &raw("Row_B", &cells)).unwrap();
        t.push_row(&ok).unwrap();
        assert_eq!(t.rows.len(), 2);
    }

    #[test]
    fn push_row_rejects_unknown_column() {
        let schema =
            Schema::from_toml_str("name='t'\nkey_column='---'\n[[fields]]\nname='Z'\ntype='int'\n")
                .unwrap();
        let mut t = parse("---,A\n").unwrap();
        let row = validate_row(
            &schema,
            &RawRow {
                line: 0,
                key: "k".into(),
                cells: vec![("Z".into(), "1".into())],
            },
        )
        .unwrap();
        let e = t.push_row(&row).unwrap_err();
        assert!(matches!(e, WriteError::UnknownColumn { ref column, .. } if column == "Z"));
        assert!(e.to_string().contains("colonnes: A"));
        assert!(t.rows.is_empty());
    }

    #[test]
    fn programmatic_cells_are_quoted_when_required() {
        let mut t = parse("---,A\n").unwrap();
        t.rows.push(CsvRecord {
            line: 2,
            cells: vec![
                CsvCell {
                    text: "a,b".into(),
                    quoted: false,
                },
                CsvCell {
                    text: "q\"".into(),
                    quoted: true,
                },
            ],
        });
        assert_eq!(t.to_bytes(), b"---,A\n\"a,b\",\"q\"\"\"\n");
        assert!(Table::parse(&t.to_bytes()).is_ok());
    }

    #[test]
    fn push_row_after_file_without_final_newline() {
        let schema =
            Schema::from_toml_str("name='t'\nkey_column='---'\n[[fields]]\nname='A'\ntype='int'\n")
                .unwrap();
        let mut t = parse("---,A\n1,\"2\"").unwrap();
        let row = validate_row(
            &schema,
            &RawRow {
                line: 0,
                key: "2".into(),
                cells: vec![("A".into(), "3".into())],
            },
        )
        .unwrap();
        t.push_row(&row).unwrap();
        assert_eq!(t.to_bytes(), b"---,A\n1,\"2\"\n2,\"3\"");
    }

    fn utf16le(text: &str) -> Vec<u8> {
        let mut v = vec![0xFF, 0xFE];
        v.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        v
    }

    #[test]
    fn encoding_is_detected_by_the_bom_and_the_bom_is_not_part_of_the_text() {
        let text = "---,A\r\n1,\"épée, \"\"x\"\" ⚔\"\r\n";
        let u16t = Table::parse(&utf16le(text)).unwrap();
        assert_eq!(u16t.encoding, Encoding::Utf16LeBom);
        assert_eq!(u16t.newline, Newline::CrLf);
        assert_eq!(u16t.header.cells[0].text, "---");
        assert_eq!(u16t.rows[0].cells[1].text, "épée, \"x\" ⚔");
        let u8b = Table::parse(&[&UTF8_BOM[..], text.as_bytes()].concat()).unwrap();
        assert_eq!(u8b.encoding, Encoding::Utf8Bom);
        assert_eq!(u8b.rows[0].cells[1].text, "épée, \"x\" ⚔");
        let plain = Table::parse(text.as_bytes()).unwrap();
        assert_eq!(plain.encoding, Encoding::Utf8);
        // même contenu, trois encodages
        assert_eq!(u16t.rows, u8b.rows);
        assert_eq!(u8b.rows, plain.rows);
    }

    #[test]
    fn every_encoding_serializes_back_to_the_exact_bytes() {
        let text = "---,A\r\n1,\"épée, \"\"x\"\" ⚔\"\r\n";
        for bytes in [
            utf16le(text),
            [&UTF8_BOM[..], text.as_bytes()].concat(),
            text.as_bytes().to_vec(),
            // astral (paire de substitution) et absence de retour final
            utf16le("---,A\n1,\"𝕊 rune ᚠ\""),
        ] {
            assert_eq!(Table::parse(&bytes).unwrap().to_bytes(), bytes);
        }
    }

    #[test]
    fn utf16_with_an_odd_length_is_an_error_naming_the_encoding() {
        let mut bytes = utf16le("---,A\n1,2\n");
        bytes.pop();
        let e = Table::parse(&bytes).unwrap_err();
        let CsvError::InvalidEncoding {
            encoding,
            offset,
            ref detail,
        } = e
        else {
            panic!("{e}")
        };
        assert_eq!(encoding, "UTF-16 LE avec BOM");
        assert_eq!(offset, bytes.len() - 1);
        assert!(detail.contains("impaire"), "{detail}");
        let message = e.to_string();
        assert!(
            message.contains("UTF-16 LE avec BOM") && message.contains("impaire"),
            "{message}"
        );
    }

    #[test]
    fn utf16_with_a_lone_surrogate_reports_its_offset() {
        let mut bytes = utf16le("---,A\n1,x");
        // unité haute de substitution sans sa suivante, en fin de fichier
        bytes.extend([0x3D, 0xD8]);
        let e = Table::parse(&bytes).unwrap_err();
        let CsvError::InvalidEncoding {
            encoding,
            offset,
            ref detail,
        } = e
        else {
            panic!("{e}")
        };
        assert_eq!(encoding, "UTF-16 LE avec BOM");
        assert_eq!(offset, bytes.len() - 2);
        assert!(detail.contains("0xD83D"), "{detail}");
    }

    #[test]
    fn invalid_utf8_after_a_bom_counts_the_bom_in_the_offset_and_names_the_encoding() {
        let e = Table::parse(b"\xef\xbb\xbf---,A\n1,\xff\n").unwrap_err();
        assert!(
            matches!(
                e,
                CsvError::InvalidEncoding {
                    encoding: "UTF-8 avec BOM",
                    offset: 11,
                    ..
                }
            ),
            "{e}"
        );
    }

    #[test]
    fn utf16_big_endian_is_refused_by_name_and_an_empty_utf16_file_is_empty() {
        let e = Table::parse(&[0xFE, 0xFF, 0x00, 0x2D]).unwrap_err();
        assert!(
            matches!(e, CsvError::UnsupportedEncoding { bom: "FE FF", .. }),
            "{e}"
        );
        assert!(e.to_string().contains("UTF-16 BE"));
        assert_eq!(Table::parse(&[0xFF, 0xFE]).unwrap_err(), CsvError::Empty);
    }
}
