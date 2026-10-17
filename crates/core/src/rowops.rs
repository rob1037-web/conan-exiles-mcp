//! Opérations par ligne (ajouter, modifier, supprimer) sur une DataTable CSV de `CSV_ROOT`.
//!
//! Chaque opération lit le CSV existant, le valide (refus total s'il est invalide: aucune
//! réparation), applique la modification en mémoire en conservant l'ordre des lignes, puis
//! passe par `CsvStore::write_with_schema`: validation de la **table entière** résultante,
//! format du Devkit, diff par valeurs, sauvegarde `.backup` horodatée, écriture atomique.
//! Aucune sérialisation n'est dupliquée ici. Le dry-run et l'écriture réelle renvoient le
//! même diff.
//!
//! Les clés sont comparées sans tenir compte de la casse ASCII (noms de ligne Unreal = FName);
//! la casse fournie est conservée à l'ajout, la casse existante à la modification.

use crate::csv::{validate_table, CsvCell, CsvRecord, Table};
use crate::schema::Schema;
use crate::store::{CsvStore, SchemaWrite, StoreError, WriteOptions};
use crate::{diff::TableDiff, store::WriteOutcome};

/// Opération sur une ligne. `values`: (nom de colonne, texte) ; la colonne clé n'en fait pas partie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowOp {
    /// La clé ne doit pas exister ; toutes les colonnes du schéma sont fournies.
    Add {
        key: String,
        values: Vec<(String, String)>,
    },
    /// La clé doit exister ; `values` est partiel et fusionné avec la ligne existante.
    Update {
        key: String,
        values: Vec<(String, String)>,
    },
    /// La clé doit exister.
    Delete { key: String },
}

impl RowOp {
    pub fn key(&self) -> &str {
        match self {
            RowOp::Add { key, .. } | RowOp::Update { key, .. } | RowOp::Delete { key } => key,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowOutcome {
    /// Clé telle qu'elle est (ou sera) écrite dans le fichier.
    pub key: String,
    /// La fusion ne change aucune valeur: rien n'est écrit, aucune sauvegarde.
    pub no_change: bool,
    pub write: SchemaWrite,
}

impl RowOutcome {
    pub fn outcome(&self) -> &WriteOutcome {
        &self.write.outcome
    }
}

fn same_key(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

impl CsvStore {
    /// Applique une opération par ligne au CSV `rel`. Voir la documentation du module.
    pub fn apply_row_op(
        &self,
        schema: &Schema,
        rel: &str,
        op: &RowOp,
        options: WriteOptions,
    ) -> Result<RowOutcome, StoreError> {
        let bytes = self.read_csv(rel)?;
        let table = Table::parse(&bytes).map_err(|source| StoreError::ExistingUnreadable {
            path: rel.into(),
            source,
        })?;
        if let Err(errors) = validate_table(schema, &table) {
            return Err(StoreError::SourceInvalid {
                table: schema.name.clone(),
                path: rel.to_owned(),
                errors,
            });
        }

        let position = table
            .rows
            .iter()
            .position(|r| same_key(&r.cells[0].text, op.key()));
        let mut new = table.clone();
        let stored_key = match op {
            RowOp::Add { key, values } => {
                if let Some(i) = position {
                    return Err(StoreError::KeyExists {
                        table: schema.name.clone(),
                        key: key.clone(),
                        existing: table.rows[i].cells[0].text.clone(),
                        line: table.rows[i].line,
                    });
                }
                let cells = check_columns(schema, key, values, true)?;
                let mut row = vec![CsvCell {
                    text: key.clone(),
                    quoted: false,
                }];
                for field in &schema.fields {
                    let text = cells
                        .iter()
                        .find(|(c, _)| *c == field.name)
                        .map(|(_, t)| t.clone())
                        .unwrap_or_default();
                    row.push(CsvCell { text, quoted: true });
                }
                new.rows.push(CsvRecord {
                    line: 0,
                    cells: row,
                });
                key.clone()
            }
            RowOp::Update { key, values } => {
                let i = position.ok_or_else(|| key_not_found(schema, &table, key))?;
                let cells = check_columns(schema, key, values, false)?;
                for (col, text) in cells {
                    if let Some(idx) = schema.fields.iter().position(|f| f.name == *col) {
                        new.rows[i].cells[idx + 1] = CsvCell {
                            text: text.clone(),
                            quoted: true,
                        };
                    }
                }
                table.rows[i].cells[0].text.clone()
            }
            RowOp::Delete { key } => {
                let i = position.ok_or_else(|| key_not_found(schema, &table, key))?;
                let removed = new.rows.remove(i);
                removed.cells[0].text.clone()
            }
        };

        // Une modification qui ne change aucune valeur n'écrit rien (ni fichier, ni sauvegarde).
        let no_value_change = matches!(op, RowOp::Update { .. })
            && !TableDiff::between(Some(&table), &new.to_devkit_format(schema)).has_value_changes();
        let new_bytes = new.to_bytes();
        let write = if no_value_change {
            self.write_with_schema(
                schema,
                rel,
                &new_bytes,
                WriteOptions {
                    dry_run: true,
                    confirm: false,
                },
            )?
        } else {
            self.write_with_schema(schema, rel, &new_bytes, options)?
        };
        Ok(RowOutcome {
            key: stored_key,
            no_change: no_value_change,
            write,
        })
    }
}

fn key_not_found(schema: &Schema, table: &Table, key: &str) -> StoreError {
    const SHOWN: usize = 10;
    let mut known: Vec<&str> = table
        .rows
        .iter()
        .take(SHOWN)
        .map(|r| r.cells[0].text.as_str())
        .collect();
    let mut known_text = known.join(", ");
    if table.rows.len() > SHOWN {
        known.push("…");
        known_text.push_str(&format!(", … ({} au total)", table.rows.len()));
    }
    if table.rows.is_empty() {
        known_text = "(table vide)".into();
    }
    StoreError::KeyNotFound {
        table: schema.name.clone(),
        key: key.to_owned(),
        known: known_text,
    }
}

/// Vérifie les noms de colonnes fournis: colonne clé (refus explicite), inconnues, doublons,
/// et, pour un ajout (`complete`), colonnes manquantes. Renvoie les couples validés.
fn check_columns<'a>(
    schema: &Schema,
    key: &str,
    values: &'a [(String, String)],
    complete: bool,
) -> Result<&'a [(String, String)], StoreError> {
    if values.iter().any(|(c, _)| *c == schema.key_column) {
        return Err(StoreError::KeyNotEditable {
            key_column: schema.key_column.clone(),
        });
    }
    let mut details = String::new();
    let unknown: Vec<String> = values
        .iter()
        .filter(|(c, _)| schema.field(c).is_none())
        .map(|(c, _)| {
            match schema
                .fields
                .iter()
                .find(|f| f.name.eq_ignore_ascii_case(c))
            {
                Some(f) => format!("{c:?} (casse différente: {:?})", f.name),
                None => format!("{c:?}"),
            }
        })
        .collect();
    if !unknown.is_empty() {
        details.push_str(&format!(" colonne(s) inconnue(s): {}.", unknown.join(", ")));
    }
    let duplicated: Vec<&str> = values
        .iter()
        .enumerate()
        .filter(|(i, (c, _))| values[..*i].iter().any(|(d, _)| d == c))
        .map(|(_, (c, _))| c.as_str())
        .collect();
    if !duplicated.is_empty() {
        details.push_str(&format!(
            " colonne(s) en double: {}.",
            duplicated.join(", ")
        ));
    }
    if complete {
        let missing: Vec<&str> = schema
            .fields
            .iter()
            .filter(|f| !values.iter().any(|(c, _)| *c == f.name))
            .map(|f| f.name.as_str())
            .collect();
        if !missing.is_empty() {
            details.push_str(&format!(
                " colonne(s) manquante(s) (toutes doivent être fournies): {}.",
                missing.join(", ")
            ));
        }
    }
    if !details.is_empty() {
        return Err(StoreError::BadColumns {
            table: schema.name.clone(),
            key: key.to_owned(),
            details,
            expected: schema
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        });
    }
    Ok(values)
}
