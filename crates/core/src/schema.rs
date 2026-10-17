//! Schémas de DataTable : lecture d'un fichier TOML vers un modèle typé.
//!
//! Format d'un schéma :
//!
//! ```toml
//! name = "weapon_items"
//! description = "Armes"            # optionnel
//!
//! [[fields]]
//! name = "Damage"
//! type = "int"                     # string | int | float | bool | enum | asset_ref | raw
//! required = true                  # défaut : false
//! min = 0                          # int / float
//! max = 1000
//! ```
//!
//! La colonne `Name` (clé de ligne Unreal) est implicite et ne se déclare pas.
//! Toute clé ou tout type inconnu est une erreur : rien ne passe en silence.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Value;

/// Nom de la colonne clé des DataTables Unreal, implicite dans tout schéma.
pub const NAME_COLUMN: &str = "Name";

/// Types reconnus dans `type = "..."`.
const KNOWN_TYPES: &[&str] = &["string", "int", "float", "bool", "enum", "asset_ref", "raw"];

#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("schéma {path}: lecture impossible: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("TOML invalide: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("champ '{field}': type inconnu '{found}' (types connus: {})", KNOWN_TYPES.join(", "))]
    UnknownType { field: String, found: String },
    #[error("champ '{field}': la clé '{key}' ne s'applique pas au type '{ty}' (clés admises: {allowed})")]
    KeyNotApplicable {
        field: String,
        ty: &'static str,
        key: &'static str,
        allowed: &'static str,
    },
    #[error("champ '{field}': '{key}' doit être {expected}, reçu {found}")]
    BadKeyType {
        field: String,
        key: &'static str,
        expected: &'static str,
        found: String,
    },
    #[error("champ '{field}': bornes incohérentes, {min_key} ({min}) > {max_key} ({max})")]
    BadBounds {
        field: String,
        min_key: &'static str,
        max_key: &'static str,
        min: String,
        max: String,
    },
    #[error("champ '{field}': le type enum exige une liste 'values' non vide")]
    EnumWithoutValues { field: String },
    #[error("champ '{field}': valeur d'enum en double: '{value}'")]
    DuplicateEnumValue { field: String, value: String },
    #[error("champ '{field}': nom déjà utilisé par le champ '{other}' (comparaison insensible à la casse)")]
    DuplicateField { field: String, other: String },
    #[error("champ '{field}': nom réservé, la colonne '{NAME_COLUMN}' est implicite")]
    ReservedField { field: String },
    #[error("champ sans nom (le nom ne peut pas être vide)")]
    EmptyFieldName,
    #[error("le nom du schéma ne peut pas être vide")]
    EmptySchemaName,
}

/// Un schéma de DataTable.
#[derive(Debug, Clone, PartialEq)]
pub struct Schema {
    pub name: String,
    pub description: Option<String>,
    pub fields: Vec<Field>,
}

/// Une colonne déclarée (hors `Name`).
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
    pub ty: FieldType,
}

/// Type d'une colonne.
///
/// Les types composites (`array`, `struct`) s'ajouteront comme variantes portant
/// des `FieldType` imbriqués (ex. `Array { item: Box<FieldType> }`,
/// `Struct { fields: Vec<Field> }`) ; d'où `non_exhaustive` et l'absence de
/// contraintes portées par `Field` : tout vit dans `FieldType`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FieldType {
    String {
        min_len: Option<usize>,
        max_len: Option<usize>,
    },
    Int {
        min: Option<i64>,
        max: Option<i64>,
    },
    Float {
        min: Option<f64>,
        max: Option<f64>,
    },
    Bool,
    Enum {
        values: Vec<String>,
    },
    AssetRef,
    /// Conservé tel quel, sans validation, réécrit à l'identique.
    Raw,
}

impl FieldType {
    /// Nom du type tel qu'écrit dans le schéma.
    pub fn type_name(&self) -> &'static str {
        match self {
            FieldType::String { .. } => "string",
            FieldType::Int { .. } => "int",
            FieldType::Float { .. } => "float",
            FieldType::Bool => "bool",
            FieldType::Enum { .. } => "enum",
            FieldType::AssetRef => "asset_ref",
            FieldType::Raw => "raw",
        }
    }
}

// --- Représentation brute du TOML -------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSchema {
    name: String,
    description: Option<String>,
    #[serde(default)]
    fields: Vec<RawField>,
}

/// Les contraintes sont lues comme des valeurs TOML brutes, puis converties selon
/// le type : cela permet de refuser une contrainte mal placée ou mal typée avec un
/// message précis, au lieu d'une erreur serde générique.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawField {
    name: String,
    #[serde(rename = "type")]
    ty: String,
    description: Option<String>,
    #[serde(default)]
    required: bool,
    min: Option<Value>,
    max: Option<Value>,
    min_len: Option<Value>,
    max_len: Option<Value>,
    values: Option<Vec<String>>,
}

impl Schema {
    /// Parse un schéma depuis du texte TOML.
    pub fn from_toml_str(text: &str) -> Result<Schema, SchemaError> {
        let raw: RawSchema = toml::from_str(text)?;
        if raw.name.trim().is_empty() {
            return Err(SchemaError::EmptySchemaName);
        }
        let mut fields: Vec<Field> = Vec::with_capacity(raw.fields.len());
        for rf in raw.fields {
            let field = convert_field(rf)?;
            if field.name == NAME_COLUMN {
                return Err(SchemaError::ReservedField { field: field.name });
            }
            if let Some(other) = fields
                .iter()
                .find(|f| f.name.eq_ignore_ascii_case(&field.name))
            {
                return Err(SchemaError::DuplicateField {
                    field: field.name,
                    other: other.name.clone(),
                });
            }
            fields.push(field);
        }
        Ok(Schema {
            name: raw.name,
            description: raw.description,
            fields,
        })
    }

    /// Lit et parse un fichier de schéma.
    pub fn from_file(path: &Path) -> Result<Schema, SchemaError> {
        let text = std::fs::read_to_string(path).map_err(|source| SchemaError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Schema::from_toml_str(&text)
    }

    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }
}

// --- Conversion brut -> modèle ----------------------------------------------------

fn convert_field(rf: RawField) -> Result<Field, SchemaError> {
    if rf.name.trim().is_empty() {
        return Err(SchemaError::EmptyFieldName);
    }
    let ty = convert_type(&rf)?;
    Ok(Field {
        name: rf.name,
        description: rf.description,
        required: rf.required,
        ty,
    })
}

fn convert_type(rf: &RawField) -> Result<FieldType, SchemaError> {
    let f = rf.name.as_str();
    match rf.ty.as_str() {
        "string" => {
            reject(rf, "string", &["min", "max", "values"], "min_len, max_len")?;
            let min_len = opt_usize(f, "min_len", &rf.min_len)?;
            let max_len = opt_usize(f, "max_len", &rf.max_len)?;
            check_order(f, "min_len", "max_len", min_len, max_len)?;
            Ok(FieldType::String { min_len, max_len })
        }
        "int" => {
            reject(rf, "int", &["min_len", "max_len", "values"], "min, max")?;
            let min = opt_i64(f, "min", &rf.min)?;
            let max = opt_i64(f, "max", &rf.max)?;
            check_order(f, "min", "max", min, max)?;
            Ok(FieldType::Int { min, max })
        }
        "float" => {
            reject(rf, "float", &["min_len", "max_len", "values"], "min, max")?;
            let min = opt_f64(f, "min", &rf.min)?;
            let max = opt_f64(f, "max", &rf.max)?;
            check_order(f, "min", "max", min, max)?;
            Ok(FieldType::Float { min, max })
        }
        "bool" => {
            reject(rf, "bool", &["min", "max", "min_len", "max_len", "values"], "aucune")?;
            Ok(FieldType::Bool)
        }
        "enum" => {
            reject(rf, "enum", &["min", "max", "min_len", "max_len"], "values")?;
            let values = rf.values.clone().unwrap_or_default();
            if values.is_empty() {
                return Err(SchemaError::EnumWithoutValues { field: f.to_owned() });
            }
            for (i, v) in values.iter().enumerate() {
                if values[..i].contains(v) {
                    return Err(SchemaError::DuplicateEnumValue {
                        field: f.to_owned(),
                        value: v.clone(),
                    });
                }
            }
            Ok(FieldType::Enum { values })
        }
        "asset_ref" => {
            reject(rf, "asset_ref", &["min", "max", "min_len", "max_len", "values"], "aucune")?;
            Ok(FieldType::AssetRef)
        }
        "raw" => {
            reject(rf, "raw", &["min", "max", "min_len", "max_len", "values"], "aucune")?;
            Ok(FieldType::Raw)
        }
        other => Err(SchemaError::UnknownType {
            field: f.to_owned(),
            found: other.to_owned(),
        }),
    }
}

/// Refuse les clés qui ne s'appliquent pas au type.
fn reject(
    rf: &RawField,
    ty: &'static str,
    forbidden: &[&'static str],
    allowed: &'static str,
) -> Result<(), SchemaError> {
    for &key in forbidden {
        let present = match key {
            "min" => rf.min.is_some(),
            "max" => rf.max.is_some(),
            "min_len" => rf.min_len.is_some(),
            "max_len" => rf.max_len.is_some(),
            "values" => rf.values.is_some(),
            _ => false,
        };
        if present {
            return Err(SchemaError::KeyNotApplicable {
                field: rf.name.clone(),
                ty,
                key,
                allowed,
            });
        }
    }
    Ok(())
}

fn describe(v: &Value) -> String {
    format!("{} ({v})", v.type_str())
}

fn opt_i64(field: &str, key: &'static str, v: &Option<Value>) -> Result<Option<i64>, SchemaError> {
    match v {
        None => Ok(None),
        Some(Value::Integer(i)) => Ok(Some(*i)),
        Some(other) => Err(SchemaError::BadKeyType {
            field: field.to_owned(),
            key,
            expected: "un entier",
            found: describe(other),
        }),
    }
}

fn opt_usize(
    field: &str,
    key: &'static str,
    v: &Option<Value>,
) -> Result<Option<usize>, SchemaError> {
    match v {
        None => Ok(None),
        Some(Value::Integer(i)) if *i >= 0 => Ok(usize::try_from(*i).ok()),
        Some(other) => Err(SchemaError::BadKeyType {
            field: field.to_owned(),
            key,
            expected: "un entier positif ou nul",
            found: describe(other),
        }),
    }
}

/// Un entier TOML est accepté comme borne de flottant (`min = 0`).
fn opt_f64(field: &str, key: &'static str, v: &Option<Value>) -> Result<Option<f64>, SchemaError> {
    match v {
        None => Ok(None),
        Some(Value::Float(x)) if x.is_finite() => Ok(Some(*x)),
        Some(Value::Integer(i)) => Ok(Some(*i as f64)),
        Some(other) => Err(SchemaError::BadKeyType {
            field: field.to_owned(),
            key,
            expected: "un nombre fini",
            found: describe(other),
        }),
    }
}

fn check_order<T: PartialOrd + ToString>(
    field: &str,
    min_key: &'static str,
    max_key: &'static str,
    min: Option<T>,
    max: Option<T>,
) -> Result<(), SchemaError> {
    if let (Some(lo), Some(hi)) = (min, max) {
        if lo > hi {
            return Err(SchemaError::BadBounds {
                field: field.to_owned(),
                min_key,
                max_key,
                min: lo.to_string(),
                max: hi.to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Schema, SchemaError> {
        Schema::from_toml_str(text)
    }

    fn one_field(body: &str) -> Result<Schema, SchemaError> {
        parse(&format!("name = \"t\"\n[[fields]]\nname = \"F\"\n{body}\n"))
    }

    #[test]
    fn parses_all_types() {
        let s = parse(
            r#"
name = "demo"
description = "d"

[[fields]]
name = "Label"
type = "string"
required = true
min_len = 1
max_len = 20

[[fields]]
name = "Damage"
type = "int"
min = 0
max = 100

[[fields]]
name = "Weight"
type = "float"
min = 0
max = 2.5

[[fields]]
name = "Stackable"
type = "bool"

[[fields]]
name = "Tier"
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
        .unwrap();
        assert_eq!(s.name, "demo");
        assert_eq!(s.fields.len(), 7);
        assert!(s.field("Label").unwrap().required);
        assert!(!s.field("Damage").unwrap().required);
        assert_eq!(
            s.field("Damage").unwrap().ty,
            FieldType::Int { min: Some(0), max: Some(100) }
        );
        assert_eq!(
            s.field("Weight").unwrap().ty,
            FieldType::Float { min: Some(0.0), max: Some(2.5) }
        );
        assert_eq!(s.field("Extra").unwrap().ty, FieldType::Raw);
        assert_eq!(s.field("Extra").unwrap().ty.type_name(), "raw");
    }

    #[test]
    fn field_order_is_preserved() {
        let s = parse(
            "name='t'\n[[fields]]\nname='B'\ntype='raw'\n[[fields]]\nname='A'\ntype='raw'\n",
        )
        .unwrap();
        let names: Vec<_> = s.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["B", "A"]);
    }

    #[test]
    fn unknown_type_is_an_error() {
        let e = one_field("type = \"array\"").unwrap_err();
        assert!(matches!(e, SchemaError::UnknownType { ref found, .. } if found == "array"));
        let msg = e.to_string();
        assert!(msg.contains("'F'") && msg.contains("'array'") && msg.contains("raw"));
    }

    #[test]
    fn missing_type_is_an_error() {
        assert!(matches!(one_field(""), Err(SchemaError::Toml(_))));
    }

    #[test]
    fn unknown_key_is_an_error() {
        assert!(matches!(
            one_field("type = \"int\"\nmaxx = 3"),
            Err(SchemaError::Toml(_))
        ));
        assert!(matches!(
            parse("name = 't'\nfoo = 1\n"),
            Err(SchemaError::Toml(_))
        ));
    }

    #[test]
    fn constraint_on_wrong_type_is_an_error() {
        let e = one_field("type = \"bool\"\nmin = 1").unwrap_err();
        assert!(matches!(e, SchemaError::KeyNotApplicable { key: "min", .. }));
        let e = one_field("type = \"int\"\nmin_len = 1").unwrap_err();
        assert!(matches!(e, SchemaError::KeyNotApplicable { key: "min_len", .. }));
        let e = one_field("type = \"raw\"\nvalues = [\"a\"]").unwrap_err();
        assert!(matches!(e, SchemaError::KeyNotApplicable { key: "values", .. }));
    }

    #[test]
    fn bad_bound_types() {
        let e = one_field("type = \"int\"\nmin = 1.5").unwrap_err();
        assert!(matches!(e, SchemaError::BadKeyType { key: "min", .. }));
        let e = one_field("type = \"string\"\nmax_len = -1").unwrap_err();
        assert!(matches!(e, SchemaError::BadKeyType { key: "max_len", .. }));
        let e = one_field("type = \"float\"\nmax = \"x\"").unwrap_err();
        assert!(matches!(e, SchemaError::BadKeyType { key: "max", .. }));
    }

    #[test]
    fn inconsistent_bounds() {
        let e = one_field("type = \"int\"\nmin = 5\nmax = 1").unwrap_err();
        assert!(matches!(e, SchemaError::BadBounds { .. }));
        let e = one_field("type = \"string\"\nmin_len = 5\nmax_len = 1").unwrap_err();
        assert!(matches!(e, SchemaError::BadBounds { .. }));
    }

    #[test]
    fn enum_rules() {
        assert!(matches!(
            one_field("type = \"enum\""),
            Err(SchemaError::EnumWithoutValues { .. })
        ));
        assert!(matches!(
            one_field("type = \"enum\"\nvalues = []"),
            Err(SchemaError::EnumWithoutValues { .. })
        ));
        assert!(matches!(
            one_field("type = \"enum\"\nvalues = [\"a\", \"a\"]"),
            Err(SchemaError::DuplicateEnumValue { .. })
        ));
    }

    #[test]
    fn name_column_is_reserved_and_duplicates_rejected() {
        let e = parse("name='t'\n[[fields]]\nname='Name'\ntype='raw'\n").unwrap_err();
        assert!(matches!(e, SchemaError::ReservedField { .. }));
        let e = parse(
            "name='t'\n[[fields]]\nname='A'\ntype='raw'\n[[fields]]\nname='a'\ntype='raw'\n",
        )
        .unwrap_err();
        assert!(matches!(e, SchemaError::DuplicateField { .. }));
    }

    #[test]
    fn empty_names_rejected() {
        assert!(matches!(
            parse("name=' '\n"),
            Err(SchemaError::EmptySchemaName)
        ));
        let e = parse("name='t'\n[[fields]]\nname=''\ntype='raw'\n").unwrap_err();
        assert!(matches!(e, SchemaError::EmptyFieldName));
    }

    #[test]
    fn from_file_reports_missing_file() {
        let e = Schema::from_file(Path::new("/nonexistent/x.toml")).unwrap_err();
        assert!(matches!(e, SchemaError::Io { .. }));
    }
}
