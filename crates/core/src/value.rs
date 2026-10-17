//! Lignes et cellules. Chaque cellule garde son texte d'origine (`raw`) en plus de
//! sa valeur typée : l'écriture réutilise `raw`, jamais une re-sérialisation de la
//! valeur typée, pour que le round-trip reste exact à l'octet.

/// Ligne telle que reçue (CSV lu ou données du client), avant validation.
/// Toutes les valeurs sont du texte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRow {
    /// Numéro de ligne pour les messages d'erreur (1 = première ligne de données,
    /// ou l'indice fourni par l'appelant ; non interprété).
    pub line: usize,
    /// Clé de ligne (première colonne, quel que soit son en-tête dans le fichier).
    pub key: String,
    /// Cellules dans l'ordre d'origine : (colonne, texte).
    pub cells: Vec<(String, String)>,
}

/// Valeur typée d'une cellule.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// Cellule vide, admise seulement pour un champ non requis.
    Empty,
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Enum(String),
    AssetRef(String),
    /// Aucune interprétation : seul `Cell::raw` fait foi.
    Raw,
}

/// Une cellule validée.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub column: String,
    /// Texte d'origine, à réécrire tel quel.
    pub raw: String,
    pub value: Value,
}

/// Une ligne validée, cellules dans l'ordre d'origine.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub line: usize,
    pub key: String,
    pub cells: Vec<Cell>,
}

impl Row {
    pub fn cell(&self, column: &str) -> Option<&Cell> {
        self.cells.iter().find(|c| c.column == column)
    }
}
