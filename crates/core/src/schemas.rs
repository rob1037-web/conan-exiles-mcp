//! Les schémas de `SCHEMA_ROOT`: un fichier TOML par DataTable, nommé d'après la table
//! (`DT_Purge_Nordheimer` → `DT_Purge_Nordheimer.toml`), à plat dans le dossier.
//!
//! `SchemaStore` est en **lecture seule** (aucune méthode d'écriture) et passe par la même
//! sandbox que les autres racines. Un schéma malformé, au nom incohérent ou sans
//! `key_column` est une erreur bloquante, qui nomme le fichier et le champ fautif.

use std::path::{Path, PathBuf};

use crate::sandbox::{check_relative, Sandbox, SandboxError};
use crate::schema::{Schema, SchemaError};

/// Taille maximale d'un fichier de schéma.
pub const MAX_SCHEMA_BYTES: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SchemaStoreError {
    #[error("SCHEMA_ROOT {root:?} inutilisable: {reason}")]
    RootUnusable { root: PathBuf, reason: String },
    #[error("nom de table {table:?} refusé: {reason}. Attendu: le nom de la table, sans dossier ni extension (ex. DT_Purge_Nordheimer)")]
    InvalidTableName { table: String, reason: String },
    #[error("aucun schéma pour la table {table:?}: fichier {file:?} introuvable dans SCHEMA_ROOT ({root:?}). Pas de schéma, pas d'écriture")]
    NotFound {
        table: String,
        file: String,
        root: PathBuf,
    },
    #[error("schéma {file:?} malformé: {source}")]
    Malformed {
        file: String,
        source: Box<SchemaError>,
    },
    #[error("schéma {file:?} incohérent: le champ name vaut {declared:?} mais la table attendue est {expected:?} (le fichier est nommé d'après la table)")]
    NameMismatch {
        file: String,
        declared: String,
        expected: String,
    },
    #[error("schéma {file:?} incomplet: le champ key_column est obligatoire (en-tête de la colonne clé dans le CSV exporté, `---` dans les exports Devkit)")]
    KeyColumnMissing { file: String },
    #[error("schéma {file:?} trop gros ({size} octets, maximum {max})")]
    TooLarge { file: String, size: u64, max: u64 },
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error("schéma {file:?}: erreur d'entrée/sortie: {source}")]
    Io {
        file: String,
        source: std::io::Error,
    },
}

impl SchemaStoreError {
    /// Nom court et stable, pour les clients MCP.
    pub fn code(&self) -> &'static str {
        match self {
            SchemaStoreError::RootUnusable { .. } => "SchemaRootUnavailable",
            SchemaStoreError::InvalidTableName { .. } => "InvalidTableName",
            SchemaStoreError::NotFound { .. } => "SchemaMissing",
            SchemaStoreError::Malformed { .. }
            | SchemaStoreError::NameMismatch { .. }
            | SchemaStoreError::KeyColumnMissing { .. } => "SchemaMalformed",
            SchemaStoreError::TooLarge { .. } => "SchemaMalformed",
            SchemaStoreError::Sandbox(SandboxError::Escapes { .. }) => "PathEscapes",
            SchemaStoreError::Sandbox(SandboxError::WriteThroughLink { .. }) => "LinkRefused",
            SchemaStoreError::Sandbox(_) => "PathRefused",
            SchemaStoreError::Io { .. } => "IoError",
        }
    }
}

/// Une entrée de `list`: la table et le résultat du chargement de son schéma.
#[derive(Debug)]
pub struct SchemaEntry {
    pub table: String,
    pub file: String,
    /// `Ok` si le schéma est valide, sinon l'erreur bloquante.
    pub status: Result<Schema, SchemaStoreError>,
}

#[derive(Debug, Clone)]
pub struct SchemaStore {
    sandbox: Sandbox,
}

impl SchemaStore {
    /// `root` doit être un dossier existant. Rien n'est créé.
    pub fn open(root: &Path) -> Result<SchemaStore, SchemaStoreError> {
        if root.as_os_str().is_empty() {
            return Err(SchemaStoreError::RootUnusable {
                root: root.to_path_buf(),
                reason: "chemin vide".into(),
            });
        }
        let sandbox = Sandbox::new(root).map_err(|e| SchemaStoreError::RootUnusable {
            root: root.to_path_buf(),
            reason: e.to_string(),
        })?;
        Ok(SchemaStore { sandbox })
    }

    pub fn root(&self) -> &Path {
        self.sandbox.root()
    }

    fn file_name(table: &str) -> Result<String, SchemaStoreError> {
        let bad = |reason: &str| SchemaStoreError::InvalidTableName {
            table: table.to_owned(),
            reason: reason.to_owned(),
        };
        if table.is_empty() {
            return Err(bad("nom vide"));
        }
        if table.starts_with('.') {
            return Err(bad("un nom de table ne commence pas par '.'"));
        }
        if table.contains('/') {
            return Err(bad("séparateur '/' interdit"));
        }
        if table.to_ascii_lowercase().ends_with(".toml") {
            return Err(bad("sans l'extension .toml"));
        }
        let file = format!("{table}.toml");
        check_relative(&file).map_err(|e| match e {
            SandboxError::InvalidPath { rule, .. } => bad(&rule.to_string()),
            other => SchemaStoreError::Sandbox(other),
        })?;
        Ok(file)
    }

    /// Charge et valide le schéma de `table`.
    pub fn load(&self, table: &str) -> Result<Schema, SchemaStoreError> {
        let file = Self::file_name(table)?;
        let path = match self.sandbox.resolve_read(&file) {
            Ok(p) => p,
            Err(SandboxError::NotFound { .. }) => {
                return Err(SchemaStoreError::NotFound {
                    table: table.to_owned(),
                    file,
                    root: self.sandbox.root().to_path_buf(),
                })
            }
            Err(e) => return Err(e.into()),
        };
        let io = |source| SchemaStoreError::Io {
            file: file.clone(),
            source,
        };
        let size = std::fs::metadata(&path).map_err(io)?.len();
        if size > MAX_SCHEMA_BYTES {
            return Err(SchemaStoreError::TooLarge {
                file,
                size,
                max: MAX_SCHEMA_BYTES,
            });
        }
        let text = std::fs::read_to_string(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::InvalidData {
                SchemaStoreError::Malformed {
                    file: file.clone(),
                    source: Box::new(SchemaError::Io {
                        path: path.clone(),
                        source,
                    }),
                }
            } else {
                SchemaStoreError::Io {
                    file: file.clone(),
                    source,
                }
            }
        })?;
        let schema =
            Schema::from_toml_str(&text).map_err(|source| SchemaStoreError::Malformed {
                file: file.clone(),
                source: Box::new(source),
            })?;
        if schema.name != table {
            return Err(SchemaStoreError::NameMismatch {
                file,
                declared: schema.name,
                expected: table.to_owned(),
            });
        }
        if schema.key_column.is_none() {
            return Err(SchemaStoreError::KeyColumnMissing { file });
        }
        Ok(schema)
    }

    /// Les schémas disponibles (fichiers `*.toml` à la racine de `SCHEMA_ROOT`), triés par
    /// nom, avec le résultat du chargement de chacun. Les liens et sous-dossiers sont ignorés.
    pub fn list(&self) -> Result<Vec<SchemaEntry>, SchemaStoreError> {
        let io = |source| SchemaStoreError::Io {
            file: self.sandbox.root().display().to_string(),
            source,
        };
        let mut names = Vec::new();
        for entry in std::fs::read_dir(self.sandbox.root()).map_err(io)? {
            let entry = entry.map_err(io)?;
            let file_type = entry.file_type().map_err(io)?;
            if file_type.is_symlink() || !file_type.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".toml") {
                if !stem.is_empty() {
                    names.push(stem.to_owned());
                }
            }
        }
        names.sort();
        Ok(names
            .into_iter()
            .map(|table| SchemaEntry {
                file: format!("{table}.toml"),
                status: self.load(&table),
                table,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const GOOD: &str = "name = \"DT_X\"\nkey_column = \"---\"\n\n[[fields]]\nname = \"A\"\ntype = \"int\"\nrequired = true\n";

    fn store_with(files: &[(&str, &str)]) -> (tempfile::TempDir, SchemaStore) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("schemas");
        fs::create_dir_all(&root).unwrap();
        for (name, content) in files {
            fs::write(root.join(name), content).unwrap();
        }
        let store = SchemaStore::open(&root).unwrap();
        (tmp, store)
    }

    #[test]
    fn loads_a_valid_schema_and_lists_it() {
        let (_t, s) = store_with(&[("DT_X.toml", GOOD), ("notes.txt", "x")]);
        let schema = s.load("DT_X").unwrap();
        assert_eq!(schema.key_column.as_deref(), Some("---"));
        assert_eq!(schema.fields.len(), 1);
        let list = s.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            (list[0].table.as_str(), list[0].file.as_str()),
            ("DT_X", "DT_X.toml")
        );
        assert!(list[0].status.is_ok());
    }

    #[test]
    fn missing_schema_is_an_explicit_error() {
        let (_t, s) = store_with(&[]);
        let e = s.load("DT_Absent").unwrap_err();
        assert!(matches!(e, SchemaStoreError::NotFound { .. }));
        assert_eq!(e.code(), "SchemaMissing");
        assert!(
            e.to_string().contains("DT_Absent.toml") && e.to_string().contains("Pas de schéma")
        );
    }

    #[test]
    fn malformed_schemas_name_the_file_and_the_faulty_field() {
        let cases = [
            ("name = \"DT_X\"\nkey_column = \"---\"\n[[fields]]\nname = \"A\"\ntype = \"quantum\"\n", "quantum"),
            ("name = \"DT_X\"\nkey_column = \"---\"\n[[fields]]\nname = \"A\"\ntype = \"enum\"\n", "'A'"),
            ("name = \"DT_X\"\nkey_column = \"---\"\n[[fields]]\nname = \"A\"\ntype = \"int\"\nmin = 5\nmax = 1\n", "'A'"),
            ("name = \"DT_X\"\nkey_column = \"---\"\n[[fields]]\nname = \"A\"\ntype = \"bool\"\nmin = 1\n", "'A'"),
            ("name = \"DT_X\"\nkey_column = \"---\"\n[[fields]]\nname = \"A\"\ntype = \"int\"\n[[fields]]\nname = \"a\"\ntype = \"int\"\n", "'a'"),
            ("name = \"DT_X\"\nkey_column = \"---\"\nbogus = 1\n", "bogus"),
            ("name = \"DT_X\"\nkey_column = \"\"\n", "key_column"),
            ("name = \"DT_X\" this is not toml", "TOML"),
        ];
        for (content, needle) in cases {
            let (_t, s) = store_with(&[("DT_X.toml", content)]);
            let e = s.load("DT_X").unwrap_err();
            assert!(
                matches!(e, SchemaStoreError::Malformed { .. }),
                "{content}: {e}"
            );
            assert_eq!(e.code(), "SchemaMalformed");
            let msg = e.to_string();
            assert!(
                msg.contains("DT_X.toml") && msg.contains(needle),
                "{needle}: {msg}"
            );
            // et le listage le signale sans s'arrêter
            assert!(s.list().unwrap()[0].status.is_err());
        }
    }

    #[test]
    fn name_mismatch_and_missing_key_column_are_blocking() {
        let (_t, s) = store_with(&[
            ("DT_X.toml", "name = \"DT_Y\"\nkey_column = \"---\"\n"),
            ("DT_Z.toml", "name = \"DT_Z\"\n"),
        ]);
        assert!(matches!(
            s.load("DT_X"),
            Err(SchemaStoreError::NameMismatch { .. })
        ));
        let e = s.load("DT_Z").unwrap_err();
        assert!(matches!(e, SchemaStoreError::KeyColumnMissing { .. }));
        assert!(e.to_string().contains("key_column"));
    }

    #[test]
    fn table_names_cannot_escape_schema_root() {
        let (t, s) = store_with(&[("DT_X.toml", GOOD)]);
        fs::write(t.path().join("secret.toml"), GOOD).unwrap();
        for table in [
            "../secret",
            "..",
            "a/b",
            "/etc/passwd",
            "C:/x",
            "..\\secret",
            "x.toml",
            "NUL",
            "",
            ".",
            "a:b",
        ] {
            let e = s.load(table).unwrap_err();
            assert!(
                matches!(
                    e,
                    SchemaStoreError::InvalidTableName { .. } | SchemaStoreError::Sandbox(_)
                ),
                "{table:?}: {e}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_schema_file_or_folder_linking_outside_is_refused() {
        let (t, s) = store_with(&[]);
        let outside = t.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("DT_L.toml"), GOOD.replace("DT_X", "DT_L")).unwrap();
        std::os::unix::fs::symlink(outside.join("DT_L.toml"), s.root().join("DT_L.toml")).unwrap();
        let e = s.load("DT_L").unwrap_err();
        assert!(
            matches!(e, SchemaStoreError::Sandbox(SandboxError::Escapes { .. })),
            "{e}"
        );
        assert_eq!(e.code(), "PathEscapes");
        assert!(s.list().unwrap().is_empty(), "les liens ne sont pas listés");
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_inside_schema_root_does_not_leak_outside_schemas() {
        let (t, s) = store_with(&[]);
        let outside = t.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("DT_L.toml"), GOOD.replace("DT_X", "DT_L")).unwrap();
        let out = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(s.root().join("sub"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(out.status.success());
        // les noms de table ne traversent pas de sous-dossier, la jonction n'est pas listée
        assert!(s.load("sub/DT_L").is_err());
        assert!(s.list().unwrap().is_empty());
    }

    #[test]
    fn unusable_roots() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            SchemaStore::open(&tmp.path().join("absent")),
            Err(SchemaStoreError::RootUnusable { .. })
        ));
        assert!(matches!(
            SchemaStore::open(Path::new("")),
            Err(SchemaStoreError::RootUnusable { .. })
        ));
        let f = tmp.path().join("f.toml");
        fs::write(&f, "").unwrap();
        assert!(matches!(
            SchemaStore::open(&f),
            Err(SchemaStoreError::RootUnusable { .. })
        ));
    }
}
