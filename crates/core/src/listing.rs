//! Listage de fichiers sous une racine (sandbox), sans jamais exposer les fichiers internes :
//! le dossier `.backup` (à toute profondeur) et les temporaires d'écriture atomique
//! `.<nom>.tmp-*`. Les liens symboliques et jonctions ne sont ni suivis ni listés.

use crate::sandbox::{Sandbox, SandboxError};
use std::path::Path;

/// Nombre maximal d'entrées renvoyées par un listage.
pub const MAX_LIST_ENTRIES: usize = 5000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Chemin relatif à la racine, séparateur `/` : réutilisable tel quel comme paramètre.
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    /// Triés dans l'ordre du parcours (dossiers en ordre alphabétique de nom).
    pub entries: Vec<FileEntry>,
    /// Le listage a été coupé à `MAX_LIST_ENTRIES` : préciser un sous-dossier.
    pub truncated: bool,
}

/// Fichier ou dossier interne, à ne jamais lister.
pub fn is_internal(name: &str, is_dir: bool) -> bool {
    if is_dir {
        name.eq_ignore_ascii_case(crate::store::BACKUP_DIR)
    } else {
        name.starts_with('.') && name.contains(".tmp-")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ListError {
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error("{path:?} est un dossier interne (sauvegardes), il n'est jamais listé")]
    InternalFolder { path: String },
    #[error("{path:?} n'est pas un dossier")]
    NotADirectory { path: String },
    #[error("erreur d'entrée/sortie en listant {at:?}: {source}")]
    Io {
        at: std::path::PathBuf,
        source: std::io::Error,
    },
}

/// Liste récursivement les fichiers sous `subfolder` (racine entière si `None` ou vide).
pub fn list_files(sandbox: &Sandbox, subfolder: Option<&str>) -> Result<Listing, ListError> {
    let start = match subfolder.filter(|s| !s.is_empty()) {
        None => sandbox.root().to_path_buf(),
        Some(sub) => {
            if sub.split('/').any(|c| is_internal(c, true)) {
                return Err(ListError::InternalFolder {
                    path: sub.to_owned(),
                });
            }
            let p = sandbox.resolve_read(sub)?;
            if !p.is_dir() {
                return Err(ListError::NotADirectory {
                    path: sub.to_owned(),
                });
            }
            p
        }
    };
    let mut listing = Listing::default();
    walk(sandbox.root(), &start, &mut listing)?;
    Ok(listing)
}

fn walk(root: &Path, dir: &Path, out: &mut Listing) -> Result<(), ListError> {
    let io = |source| ListError::Io {
        at: dir.to_path_buf(),
        source,
    };
    let mut children = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let entry = entry.map_err(io)?;
        let file_type = entry.file_type().map_err(io)?;
        children.push((entry.file_name(), entry.path(), file_type));
    }
    children.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, path, file_type) in children {
        if out.truncated {
            return Ok(());
        }
        if file_type.is_symlink() {
            continue;
        }
        let name = name.to_string_lossy();
        if file_type.is_dir() {
            if !is_internal(&name, true) {
                walk(root, &path, out)?;
            }
        } else if file_type.is_file() && !is_internal(&name, false) {
            if out.entries.len() >= MAX_LIST_ENTRIES {
                out.truncated = true;
                return Ok(());
            }
            let size = std::fs::metadata(&path).map_err(io)?.len();
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.entries.push(FileEntry { path: rel, size });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sandbox() -> (tempfile::TempDir, Sandbox) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        fs::create_dir_all(root.join("data/sub")).unwrap();
        fs::create_dir_all(root.join(".backup/data")).unwrap();
        fs::write(root.join("b.csv"), "bb").unwrap();
        fs::write(root.join("a.csv"), "a").unwrap();
        fs::write(root.join("data/c.csv"), "ccc").unwrap();
        fs::write(root.join("data/sub/d.csv"), "dddd").unwrap();
        fs::write(root.join(".backup/data/c.20260101T000000.000Z.csv"), "old").unwrap();
        fs::write(root.join(".a.csv.tmp-12-34"), "tmp").unwrap();
        fs::write(root.join("data/.c.csv.tmp-1-2"), "tmp").unwrap();
        fs::create_dir_all(root.join("data/.BACKUP")).unwrap();
        fs::write(root.join("data/.BACKUP/x.csv"), "x").unwrap();
        // un fichier légitime dont le nom ressemble sans en être: gardé
        fs::write(root.join(".gitkeep"), "").unwrap();
        fs::write(root.join("notes.tmp-1"), "n").unwrap();
        let sb = Sandbox::new(&root).unwrap();
        (tmp, sb)
    }

    fn paths(l: &Listing) -> Vec<&str> {
        l.entries.iter().map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn lists_everything_except_backup_and_temporaries() {
        let (_t, sb) = sandbox();
        let l = list_files(&sb, None).unwrap();
        assert_eq!(
            paths(&l),
            [
                ".gitkeep",
                "a.csv",
                "b.csv",
                "data/c.csv",
                "data/sub/d.csv",
                "notes.tmp-1"
            ]
        );
        assert!(!l.truncated);
        assert_eq!(l.entries[3].size, 3);
    }

    #[test]
    fn lists_a_subfolder_with_paths_relative_to_the_root() {
        let (_t, sb) = sandbox();
        let l = list_files(&sb, Some("data")).unwrap();
        assert_eq!(paths(&l), ["data/c.csv", "data/sub/d.csv"]);
        assert_eq!(paths(&list_files(&sb, Some("")).unwrap()).len(), 6);
    }

    #[test]
    fn subfolder_errors() {
        let (_t, sb) = sandbox();
        assert!(matches!(
            list_files(&sb, Some("a.csv")),
            Err(ListError::NotADirectory { .. })
        ));
        assert!(matches!(
            list_files(&sb, Some("../x")),
            Err(ListError::Sandbox(SandboxError::InvalidPath { .. }))
        ));
        assert!(matches!(
            list_files(&sb, Some("absent")),
            Err(ListError::Sandbox(SandboxError::NotFound { .. }))
        ));
    }

    #[test]
    fn the_backup_folder_is_never_listed_even_when_asked_for() {
        let (_t, sb) = sandbox();
        for sub in [".backup", ".BACKUP/data", "data/.backup"] {
            assert!(
                matches!(
                    list_files(&sb, Some(sub)),
                    Err(ListError::InternalFolder { .. })
                ),
                "{sub}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn links_are_neither_followed_nor_listed() {
        let (_t, sb) = sandbox();
        let outside = sb.root().parent().unwrap().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.csv"), "s").unwrap();
        std::os::unix::fs::symlink(&outside, sb.root().join("link")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.csv"), sb.root().join("l.csv")).unwrap();
        let l = list_files(&sb, None).unwrap();
        assert!(paths(&l)
            .iter()
            .all(|p| !p.contains("link") && !p.contains("secret") && *p != "l.csv"));
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_neither_followed_nor_listed() {
        let (_t, sb) = sandbox();
        let outside = sb.root().parent().unwrap().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.csv"), "s").unwrap();
        let out = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(sb.root().join("link"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(out.status.success());
        let l = list_files(&sb, None).unwrap();
        assert!(paths(&l)
            .iter()
            .all(|p| !p.contains("link") && !p.contains("secret")));
    }

    #[test]
    fn listing_is_truncated_at_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        fs::create_dir_all(&root).unwrap();
        for i in 0..(MAX_LIST_ENTRIES + 3) {
            fs::write(root.join(format!("f{i:05}.csv")), "").unwrap();
        }
        let sb = Sandbox::new(&root).unwrap();
        let l = list_files(&sb, None).unwrap();
        assert_eq!(l.entries.len(), MAX_LIST_ENTRIES);
        assert!(l.truncated);
        assert_eq!(l.entries[0].path, "f00000.csv");
    }
}
