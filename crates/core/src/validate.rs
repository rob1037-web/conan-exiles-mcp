//! Validation d'une ligne contre un schéma. Toutes les erreurs de la ligne sont
//! renvoyées d'un coup, chacune avec ligne, champ, valeur reçue et valeur attendue.

use std::fmt;

use crate::schema::{Field, FieldType, Schema};
use crate::value::{Cell, RawRow, Row, Value};

/// Règle violée. `code()` est stable et destiné aux clients (rapports structurés).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// Colonne de l'en-tête (ou de la ligne) inconnue du schéma.
    UnknownColumn,
    /// Colonne du schéma absente de l'en-tête (ou de la ligne).
    MissingColumn,
    DuplicateColumn,
    /// Mêmes colonnes que le schéma, dans un autre ordre.
    ColumnOrder,
    /// L'en-tête de la colonne clé ne correspond pas à `key_column`.
    KeyHeaderMismatch,
    EmptyKey,
    DuplicateKey,
    /// Cellule vide dans un champ obligatoire.
    RequiredEmpty,
    WrongType,
    OutOfRange,
    NotInEnum,
    /// Nombre de valeurs différent du nombre de colonnes.
    ColumnCount,
}

impl Rule {
    pub fn code(self) -> &'static str {
        match self {
            Rule::UnknownColumn => "UnknownColumn",
            Rule::MissingColumn => "MissingColumn",
            Rule::DuplicateColumn => "DuplicateColumn",
            Rule::ColumnOrder => "ColumnOrder",
            Rule::KeyHeaderMismatch => "KeyHeaderMismatch",
            Rule::EmptyKey => "EmptyKey",
            Rule::DuplicateKey => "DuplicateKey",
            Rule::RequiredEmpty => "RequiredEmpty",
            Rule::WrongType => "WrongType",
            Rule::OutOfRange => "OutOfRange",
            Rule::NotInEnum => "NotInEnum",
            Rule::ColumnCount => "ColumnCount",
        }
    }

    /// Erreur de structure (en-tête) plutôt que de valeur: le schéma ne correspond pas au fichier.
    pub fn is_schema_drift(self) -> bool {
        matches!(
            self,
            Rule::UnknownColumn
                | Rule::MissingColumn
                | Rule::DuplicateColumn
                | Rule::ColumnOrder
                | Rule::KeyHeaderMismatch
        )
    }
}

/// Une erreur de validation, lisible par une IA: ligne, clé de ligne, colonne, valeur reçue,
/// règle violée et valeur attendue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub line: usize,
    /// Clé de la ligne en cause ; `None` pour une erreur d'en-tête.
    pub key: Option<String>,
    /// Colonne en cause ; `None` pour la clé de ligne (première colonne).
    pub column: Option<String>,
    /// Valeur reçue ; `None` si la colonne est absente.
    pub received: Option<String>,
    pub rule: Rule,
    pub expected: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ligne {}", self.line)?;
        if let Some(k) = &self.key {
            write!(f, " (clé {k})")?;
        }
        match &self.column {
            Some(c) => write!(f, ", colonne '{c}'")?,
            None if self.key.is_some() => write!(f, ", clé de ligne")?,
            None => {}
        }
        write!(f, " [{}]", self.rule.code())?;
        match &self.received {
            Some(v) => write!(f, ": reçu {v:?}")?,
            None => write!(f, ": colonne absente")?,
        }
        write!(f, ", attendu {}", self.expected)
    }
}

impl std::error::Error for ValidationError {}

/// Rapport lisible: une ligne par erreur, au plus `limit` lignes (`0` = toutes).
pub fn render_report(errors: &[ValidationError], limit: usize) -> String {
    let shown = if limit == 0 {
        errors.len()
    } else {
        limit.min(errors.len())
    };
    let mut out = String::new();
    for e in &errors[..shown] {
        out.push_str(&format!("  - {e}\n"));
    }
    if shown < errors.len() {
        out.push_str(&format!(
            "  … et {} autre(s) erreur(s)\n",
            errors.len() - shown
        ));
    }
    out
}

/// Valide une ligne. Renvoie la ligne typée, ou toutes les erreurs trouvées.
pub fn validate_row(schema: &Schema, row: &RawRow) -> Result<Row, Vec<ValidationError>> {
    let mut errors = Vec::new();
    let err = |rule: Rule, column: Option<&str>, received: Option<&str>, expected: String| {
        ValidationError {
            line: row.line,
            key: Some(row.key.clone()),
            column: column.map(str::to_owned),
            received: received.map(str::to_owned),
            rule,
            expected,
        }
    };

    if row.key.trim().is_empty() {
        errors.push(err(
            Rule::EmptyKey,
            None,
            Some(&row.key),
            "une clé de ligne non vide".into(),
        ));
    }

    // Colonnes inconnues et doublons.
    for (i, (col, raw)) in row.cells.iter().enumerate() {
        if row.cells[..i].iter().any(|(c, _)| c == col) {
            errors.push(err(
                Rule::DuplicateColumn,
                Some(col),
                Some(raw),
                "une seule cellule par colonne".into(),
            ));
        }
        if schema.field(col).is_none() {
            let hint = schema
                .fields
                .iter()
                .find(|f| f.name.eq_ignore_ascii_case(col))
                .map(|f| format!(" (casse différente: '{}')", f.name))
                .unwrap_or_default();
            let known: Vec<_> = schema.fields.iter().map(|f| f.name.as_str()).collect();
            errors.push(err(
                Rule::UnknownColumn,
                Some(col),
                Some(raw),
                format!(
                    "une colonne du schéma '{}': {}{hint}",
                    schema.name,
                    known.join(", ")
                ),
            ));
        }
    }

    // Champs du schéma.
    let mut cells = Vec::with_capacity(row.cells.len());
    for (col, raw) in &row.cells {
        let Some(field) = schema.field(col) else {
            continue;
        };
        match check_cell(field, raw) {
            Ok(value) => cells.push(Cell {
                column: col.clone(),
                raw: raw.clone(),
                value,
            }),
            Err((rule, expected)) => errors.push(err(rule, Some(col), Some(raw), expected)),
        }
    }
    // Toutes les colonnes du schéma doivent figurer dans la ligne, requises ou non :
    // le Devkit exporte tous les champs. `required` ne porte que sur les cellules vides.
    for field in &schema.fields {
        if !row.cells.iter().any(|(c, _)| *c == field.name) {
            errors.push(err(
                Rule::MissingColumn,
                Some(&field.name),
                None,
                format!(
                    "une cellule pour cette colonne ({}; toutes les colonnes du schéma doivent figurer, une cellule vide est admise si le champ n'est pas requis)",
                    describe_type(&field.ty)
                ),
            ));
        }
    }

    if errors.is_empty() {
        Ok(Row {
            line: row.line,
            key: row.key.clone(),
            cells,
        })
    } else {
        Err(errors)
    }
}

/// Valide une cellule ; en cas d'échec, renvoie la règle violée et la description de ce
/// qui était attendu.
fn check_cell(field: &Field, raw: &str) -> Result<Value, (Rule, String)> {
    if raw.is_empty() {
        return if field.required {
            Err((
                Rule::RequiredEmpty,
                format!("une valeur non vide ({}, requis)", describe_type(&field.ty)),
            ))
        } else {
            Ok(Value::Empty)
        };
    }
    let wrong = || (Rule::WrongType, describe_type(&field.ty));
    let range = || (Rule::OutOfRange, describe_type(&field.ty));
    match &field.ty {
        FieldType::String { min_len, max_len } => {
            let n = raw.chars().count();
            if min_len.is_some_and(|m| n < m) || max_len.is_some_and(|m| n > m) {
                return Err(range());
            }
            Ok(Value::String(raw.to_owned()))
        }
        FieldType::Int { min, max } => {
            let v: i64 = raw.parse().map_err(|_| wrong())?;
            if min.is_some_and(|m| v < m) || max.is_some_and(|m| v > m) {
                return Err(range());
            }
            Ok(Value::Int(v))
        }
        FieldType::Float { min, max } => {
            let v: f64 = raw.parse().map_err(|_| wrong())?;
            if !v.is_finite() {
                return Err(wrong());
            }
            if min.is_some_and(|m| v < m) || max.is_some_and(|m| v > m) {
                return Err(range());
            }
            Ok(Value::Float(v))
        }
        FieldType::Bool => {
            if raw.eq_ignore_ascii_case("true") {
                Ok(Value::Bool(true))
            } else if raw.eq_ignore_ascii_case("false") {
                Ok(Value::Bool(false))
            } else {
                Err(wrong())
            }
        }
        FieldType::Enum { values } => {
            if values.iter().any(|v| v == raw) {
                Ok(Value::Enum(raw.to_owned()))
            } else {
                Err((Rule::NotInEnum, describe_type(&field.ty)))
            }
        }
        FieldType::AssetRef => Ok(Value::AssetRef(raw.to_owned())),
        FieldType::Raw => Ok(Value::Raw),
    }
}

/// Description lisible du type et de ses bornes, utilisée comme « valeur attendue ».
fn describe_type(ty: &FieldType) -> String {
    fn range<T: fmt::Display>(lo: &Option<T>, hi: &Option<T>, unit: &str) -> String {
        match (lo, hi) {
            (Some(a), Some(b)) => format!(" entre {a} et {b}{unit}"),
            (Some(a), None) => format!(" au moins {a}{unit}"),
            (None, Some(b)) => format!(" au plus {b}{unit}"),
            (None, None) => String::new(),
        }
    }
    match ty {
        FieldType::String { min_len, max_len } => {
            format!("un texte{}", range(min_len, max_len, " caractères"))
        }
        FieldType::Int { min, max } => format!("un entier{}", range(min, max, "")),
        FieldType::Float { min, max } => format!("un nombre{}", range(min, max, "")),
        FieldType::Bool => "true ou false".into(),
        FieldType::Enum { values } => format!("une valeur parmi: {}", values.join(", ")),
        FieldType::AssetRef => "une référence d'asset non vide".into(),
        FieldType::Raw => "n'importe quelle valeur".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Schema {
        Schema::from_toml_str(
            r#"
name = "t"
key_column = "---"
[[fields]]
name = "Label"
type = "string"
required = true
max_len = 5
[[fields]]
name = "Qty"
type = "int"
min = 0
max = 10
[[fields]]
name = "Ratio"
type = "float"
min = 0
max = 1
[[fields]]
name = "On"
type = "bool"
[[fields]]
name = "Cat"
type = "enum"
values = ["A", "B"]
[[fields]]
name = "Icon"
type = "asset_ref"
[[fields]]
name = "Extra"
type = "raw"
"#,
        )
        .unwrap()
    }

    fn row(cells: &[(&str, &str)]) -> RawRow {
        RawRow {
            line: 7,
            key: "Row_1".into(),
            cells: cells
                .iter()
                .map(|(c, v)| ((*c).into(), (*v).into()))
                .collect(),
        }
    }

    /// Ligne complétée : les colonnes du schéma absentes de `cells` reçoivent une cellule
    /// vide (valeur valide pour un champ non requis), `Label` (requis) reçoit "ok".
    fn filled(cells: &[(&str, &str)]) -> RawRow {
        let mut r = row(cells);
        for f in &schema().fields {
            if !cells.iter().any(|(c, _)| *c == f.name) {
                let v = if f.name == "Label" { "ok" } else { "" };
                r.cells.push((f.name.clone(), v.into()));
            }
        }
        r
    }

    fn errs(cells: &[(&str, &str)]) -> Vec<ValidationError> {
        validate_row(&schema(), &filled(cells)).unwrap_err()
    }

    #[test]
    fn valid_row_keeps_raw_text_and_order() {
        let r = validate_row(
            &schema(),
            &filled(&[
                ("Qty", "007"),
                ("Label", "abc"),
                ("Ratio", "0.50"),
                ("On", "TRUE"),
                ("Cat", "B"),
                ("Icon", "/Game/X.X"),
                ("Extra", "(a=1,b=\"x\")"),
            ]),
        )
        .unwrap();
        let cols: Vec<_> = r.cells.iter().map(|c| c.column.as_str()).collect();
        assert_eq!(
            cols,
            ["Qty", "Label", "Ratio", "On", "Cat", "Icon", "Extra"]
        );
        let qty = r.cell("Qty").unwrap();
        assert_eq!((qty.raw.as_str(), &qty.value), ("007", &Value::Int(7)));
        let ratio = r.cell("Ratio").unwrap();
        assert_eq!(
            (ratio.raw.as_str(), &ratio.value),
            ("0.50", &Value::Float(0.5))
        );
        let on = r.cell("On").unwrap();
        assert_eq!((on.raw.as_str(), &on.value), ("TRUE", &Value::Bool(true)));
        assert_eq!(r.cell("Extra").unwrap().value, Value::Raw);
        assert_eq!(r.cell("Extra").unwrap().raw, "(a=1,b=\"x\")");
    }

    #[test]
    fn optional_empty_cell_is_ok_required_empty_is_not() {
        let r = validate_row(&schema(), &filled(&[("Label", "a"), ("Qty", "")])).unwrap();
        assert_eq!(r.cell("Qty").unwrap().value, Value::Empty);
        let e = errs(&[("Label", "")]);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].column.as_deref(), Some("Label"));
    }

    #[test]
    fn every_schema_column_must_be_present_required_or_not() {
        // Seule "Label" est fournie : les 6 autres colonnes (dont les optionnelles) manquent.
        let e = validate_row(&schema(), &row(&[("Label", "a")])).unwrap_err();
        assert_eq!(e.len(), 6);
        assert!(e.iter().all(|x| x.received.is_none()));
        let cols: Vec<_> = e.iter().filter_map(|x| x.column.as_deref()).collect();
        assert_eq!(cols, ["Qty", "Ratio", "On", "Cat", "Icon", "Extra"]);
        assert!(e[0].to_string().contains("colonne absente"));
    }

    #[test]
    fn reports_all_errors_at_once_with_precise_messages() {
        let e = errs(&[
            ("Label", "trop long"),
            ("Qty", "11"),
            ("Ratio", "abc"),
            ("On", "oui"),
            ("Cat", "C"),
            ("Icon", ""),
        ]);
        let cols: Vec<_> = e.iter().filter_map(|x| x.column.as_deref()).collect();
        assert_eq!(cols, ["Label", "Qty", "Ratio", "On", "Cat"]);
        assert!(e.iter().all(|x| x.line == 7));
        assert_eq!(
            e[1].to_string(),
            "ligne 7 (clé Row_1), colonne 'Qty' [OutOfRange]: reçu \"11\", attendu un entier entre 0 et 10"
        );
        assert!(e[4].to_string().contains("une valeur parmi: A, B"));
    }

    #[test]
    fn int_and_float_strictness() {
        for bad in ["1.5", "1e3", " 1", "x"] {
            let e = errs(&[("Label", "a"), ("Qty", bad)]);
            assert_eq!(e[0].column.as_deref(), Some("Qty"), "{bad:?}");
        }
        for bad in ["NaN", "inf", "-inf", "1,5", "2"] {
            let e = errs(&[("Label", "a"), ("Ratio", bad)]);
            assert_eq!(e[0].column.as_deref(), Some("Ratio"), "{bad:?}");
        }
    }

    #[test]
    fn unknown_column_and_case_hint() {
        let e = errs(&[("Label", "a"), ("qty", "1"), ("Zzz", "1")]);
        assert_eq!(e.len(), 2);
        assert!(e[0].expected.contains("casse différente: 'Qty'"));
        assert!(e[1].expected.contains("Label"));
    }

    #[test]
    fn duplicate_column_and_empty_name() {
        let mut r = filled(&[("Label", "a"), ("Label", "b")]);
        r.key = "  ".into();
        let e = validate_row(&schema(), &r).unwrap_err();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].column, None);
        assert!(e[0]
            .to_string()
            .starts_with("ligne 7 (clé   ), clé de ligne [EmptyKey]"));
    }

    #[test]
    fn string_length_counts_characters() {
        // 5 caractères mais 10 octets
        assert!(validate_row(&schema(), &filled(&[("Label", "éééé!")])).is_ok());
    }
}
