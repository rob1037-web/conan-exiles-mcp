//! Différence entre deux versions d'une DataTable CSV, ligne par ligne, par clé de ligne.
//!
//! Les lignes sont appariées par leur clé (première cellule). Si une clé apparaît
//! plusieurs fois, les occurrences sont appariées dans l'ordre. L'ordre des lignes n'est
//! pas une différence de contenu (signalé à part par `order_changed`, pour les lignes communes) : seuls l'en-tête, le contenu des cellules (texte et quoting) et le
//! format du fichier (BOM, fins de ligne, retour final) le sont.

use std::collections::HashMap;

use crate::csv::{CsvCell, CsvRecord, Table};

/// Ligne ajoutée ou supprimée : sa clé et le texte de ses cellules (clé comprise).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub key: String,
    pub cells: Vec<String>,
}

/// Une cellule modifiée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnChange {
    /// Nom de la colonne dans la nouvelle version (`#<n>` si elle n'a plus d'en-tête).
    pub column: String,
    pub before: String,
    pub after: String,
    /// Seul le quoting change, pas le texte.
    pub quoting_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModifiedRow {
    pub key: String,
    pub changes: Vec<ColumnChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TableDiff {
    /// L'en-tête (texte ou quoting) diffère. Toujours `false` pour un fichier créé.
    pub header_changed: bool,
    /// BOM, type de fin de ligne ou retour final différent.
    pub format_changed: bool,
    /// Les lignes sont dans un autre ordre (sans que cela change leur contenu).
    pub order_changed: bool,
    pub added: Vec<DiffRow>,
    pub removed: Vec<DiffRow>,
    pub modified: Vec<ModifiedRow>,
}

impl TableDiff {
    /// `old` à `None` : création, toutes les lignes sont ajoutées.
    pub fn between(old: Option<&Table>, new: &Table) -> TableDiff {
        let Some(old) = old else {
            return TableDiff {
                added: new.rows.iter().map(diff_row).collect(),
                ..TableDiff::default()
            };
        };
        let mut diff = TableDiff {
            header_changed: old.header.cells != new.header.cells,
            format_changed: old.bom != new.bom
                || old.newline != new.newline
                || old.final_newline != new.final_newline,
            order_changed: common_order_differs(old, new),
            ..TableDiff::default()
        };

        let mut old_by_key: HashMap<&str, Vec<&CsvRecord>> = HashMap::new();
        for r in &old.rows {
            old_by_key.entry(&r.cells[0].text).or_default().push(r);
        }
        let mut used: HashMap<&str, usize> = HashMap::new();
        for r in &new.rows {
            let key = r.cells[0].text.as_str();
            let n = used.entry(key).or_insert(0);
            let before = old_by_key.get(key).and_then(|v| v.get(*n));
            *n += 1;
            match before {
                None => diff.added.push(diff_row(r)),
                Some(b) => {
                    let changes = cell_changes(&new.header.cells, &b.cells, &r.cells);
                    if !changes.is_empty() {
                        diff.modified.push(ModifiedRow {
                            key: key.to_owned(),
                            changes,
                        });
                    }
                }
            }
        }
        // Supprimées : occurrences d'une clé au-delà de ce que la nouvelle version contient.
        let mut new_count: HashMap<&str, usize> = HashMap::new();
        for r in &new.rows {
            *new_count.entry(&r.cells[0].text).or_insert(0) += 1;
        }
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for r in &old.rows {
            let key = r.cells[0].text.as_str();
            let n = seen.entry(key).or_insert(0);
            if *n >= new_count.get(key).copied().unwrap_or(0) {
                diff.removed.push(diff_row(r));
            }
            *n += 1;
        }
        diff
    }

    /// Aucune différence : écrire ce contenu ne changerait rien.
    pub fn is_empty(&self) -> bool {
        !self.header_changed
            && !self.format_changed
            && !self.order_changed
            && self.added.is_empty()
            && self.removed.is_empty()
            && self.modified.is_empty()
    }

    /// Résumé lisible : « 3 ajoutées, 1 modifiée, 0 supprimée ».
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} ajoutée(s), {} modifiée(s), {} supprimée(s)",
            self.added.len(),
            self.modified.len(),
            self.removed.len()
        );
        if self.header_changed {
            s.push_str(", en-tête modifié");
        }
        if self.order_changed {
            s.push_str(", ordre des lignes modifié");
        }
        if self.format_changed {
            s.push_str(", format du fichier modifié (BOM, fins de ligne ou retour final)");
        }
        s
    }
}

/// L'ordre relatif des lignes présentes dans les deux versions diffère.
fn common_order_differs(old: &Table, new: &Table) -> bool {
    let in_new: std::collections::HashSet<&str> =
        new.rows.iter().map(|r| r.cells[0].text.as_str()).collect();
    let in_old: std::collections::HashSet<&str> =
        old.rows.iter().map(|r| r.cells[0].text.as_str()).collect();
    let keys = |t: &Table, other: &std::collections::HashSet<&str>| -> Vec<String> {
        t.rows
            .iter()
            .map(|r| r.cells[0].text.clone())
            .filter(|k| other.contains(k.as_str()))
            .collect()
    };
    keys(old, &in_new) != keys(new, &in_old)
}

/// Nombre maximal de lignes détaillées par section dans `render`.
const RENDER_LIMIT: usize = 50;

impl TableDiff {
    /// Diff lisible (texte brut), détaillant au plus `RENDER_LIMIT` lignes par section.
    ///
    /// ```text
    /// Résumé: 1 ajoutée(s), 1 modifiée(s), 0 supprimée(s)
    /// Lignes ajoutées:
    ///   + 3 | 3,"z"
    /// Lignes modifiées:
    ///   ~ 2 | A: "y" -> "Y"
    /// ```
    pub fn render(&self) -> String {
        let mut out = format!("Résumé: {}\n", self.summary());
        let more = |n: usize| {
            if n > RENDER_LIMIT {
                format!("  … et {} autre(s)\n", n - RENDER_LIMIT)
            } else {
                String::new()
            }
        };
        let join = |cells: &[String]| cells.join(", ");
        if !self.added.is_empty() {
            out.push_str("Lignes ajoutées:\n");
            for r in self.added.iter().take(RENDER_LIMIT) {
                out.push_str(&format!("  + {} | {}\n", r.key, join(&r.cells)));
            }
            out.push_str(&more(self.added.len()));
        }
        if !self.modified.is_empty() {
            out.push_str("Lignes modifiées:\n");
            for r in self.modified.iter().take(RENDER_LIMIT) {
                let changes: Vec<String> = r
                    .changes
                    .iter()
                    .map(|c| {
                        if c.quoting_only {
                            format!("{}: {:?} (guillemets seuls)", c.column, c.after)
                        } else {
                            format!("{}: {:?} -> {:?}", c.column, c.before, c.after)
                        }
                    })
                    .collect();
                out.push_str(&format!("  ~ {} | {}\n", r.key, changes.join("; ")));
            }
            out.push_str(&more(self.modified.len()));
        }
        if !self.removed.is_empty() {
            out.push_str("Lignes supprimées:\n");
            for r in self.removed.iter().take(RENDER_LIMIT) {
                out.push_str(&format!("  - {} | {}\n", r.key, join(&r.cells)));
            }
            out.push_str(&more(self.removed.len()));
        }
        out
    }
}

fn diff_row(r: &CsvRecord) -> DiffRow {
    DiffRow {
        key: r.cells[0].text.clone(),
        cells: r.cells.iter().map(|c| c.text.clone()).collect(),
    }
}

fn cell_changes(header: &[CsvCell], before: &[CsvCell], after: &[CsvCell]) -> Vec<ColumnChange> {
    let mut out = Vec::new();
    for i in 0..before.len().max(after.len()) {
        let (b, a) = (before.get(i), after.get(i));
        if b == a {
            continue;
        }
        out.push(ColumnChange {
            column: header
                .get(i)
                .map_or_else(|| format!("#{i}"), |h| h.text.clone()),
            before: b.map_or_else(String::new, |c| c.text.clone()),
            after: a.map_or_else(String::new, |c| c.text.clone()),
            quoting_only: matches!((b, a), (Some(b), Some(a)) if b.text == a.text),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Table {
        Table::parse(s.as_bytes()).unwrap()
    }

    #[test]
    fn identical_tables_have_an_empty_diff() {
        let a = t("---,A\n1,\"x\"\n2,\"y\"\n");
        assert!(TableDiff::between(Some(&a), &a.clone()).is_empty());
    }

    #[test]
    fn added_modified_removed_rows() {
        let old = t("---,A,B\n1,\"x\",\"1\"\n2,\"y\",\"2\"\n3,\"z\",\"3\"\n");
        let new = t("---,A,B\n1,\"x\",\"1\"\n2,\"Y\",\"2\"\n4,\"w\",\"4\"\n");
        let d = TableDiff::between(Some(&old), &new);
        assert_eq!(
            d.added,
            [DiffRow {
                key: "4".into(),
                cells: vec!["4".into(), "w".into(), "4".into()]
            }]
        );
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.removed[0].key, "3");
        assert_eq!(d.modified.len(), 1);
        assert_eq!(d.modified[0].key, "2");
        assert_eq!(
            d.modified[0].changes,
            [ColumnChange {
                column: "A".into(),
                before: "y".into(),
                after: "Y".into(),
                quoting_only: false
            }]
        );
        assert!(!d.header_changed && !d.format_changed && !d.is_empty());
        assert_eq!(d.summary(), "1 ajoutée(s), 1 modifiée(s), 1 supprimée(s)");
    }

    #[test]
    fn row_order_is_reported_apart_from_content_changes() {
        let old = t("---,A\n1,\"x\"\n2,\"y\"\n");
        let new = t("---,A\n2,\"y\"\n1,\"x\"\n");
        let d = TableDiff::between(Some(&old), &new);
        assert!(d.order_changed && !d.is_empty());
        assert!(d.added.is_empty() && d.removed.is_empty() && d.modified.is_empty());
    }

    #[test]
    fn quoting_header_and_format_changes_are_reported() {
        let old = t("---,A\n1,\"x\"\n");
        let d = TableDiff::between(Some(&old), &t("---,A\n1,x\n"));
        assert!(d.modified[0].changes[0].quoting_only);
        let d = TableDiff::between(Some(&old), &t("---,B\n1,\"x\"\n"));
        assert!(d.header_changed);
        let d = TableDiff::between(Some(&old), &t("---,A\r\n1,\"x\"\r\n"));
        assert!(d.format_changed && d.added.is_empty() && d.modified.is_empty());
    }

    #[test]
    fn creation_lists_every_row_as_added() {
        let d = TableDiff::between(None, &t("---,A\n1,\"x\"\n2,\"y\"\n"));
        assert_eq!(d.added.len(), 2);
        assert!(!d.header_changed && d.removed.is_empty() && d.modified.is_empty());
    }

    #[test]
    fn render_is_readable_and_bounded() {
        let old = t("---,A\n1,\"x\"\n2,\"y\"\n3,\"z\"\n");
        let new = t("---,A\n1,\"x\"\n2,\"Y\"\n4,\"w\"\n");
        let text = TableDiff::between(Some(&old), &new).render();
        assert_eq!(
            text,
            "Résumé: 1 ajoutée(s), 1 modifiée(s), 1 supprimée(s)\n\
             Lignes ajoutées:\n  + 4 | 4, w\n\
             Lignes modifiées:\n  ~ 2 | A: \"y\" -> \"Y\"\n\
             Lignes supprimées:\n  - 3 | 3, z\n"
        );
        let mut big = String::from("---,A\n");
        for i in 0..120 {
            big.push_str(&format!("{i},\"v\"\n"));
        }
        let d = TableDiff::between(None, &t(&big));
        let text = d.render();
        assert!(text.contains("  + 49 | "));
        assert!(!text.contains("  + 50 | "));
        assert!(text.contains("… et 70 autre(s)"));
    }

    #[test]
    fn duplicate_keys_are_paired_in_order() {
        let old = t("---,A\n1,\"a\"\n1,\"b\"\n");
        let new = t("---,A\n1,\"a\"\n");
        let d = TableDiff::between(Some(&old), &new);
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.removed[0].cells[1], "b");
        assert!(d.added.is_empty() && d.modified.is_empty());
    }
}
