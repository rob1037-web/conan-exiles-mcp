//! Écriture des CSV sous `CSV_ROOT`, jamais sous `MOD_ROOT`.
//!
//! `CsvStore` est le seul point d'écriture de fichiers de données :
//! - tout chemin passe par la sandbox de `CSV_ROOT` (contrôle lexical, liens, jonctions) ;
//! - `MOD_ROOT` est en lecture seule : tout chemin d'écriture qui se résout sous lui est
//!   refusé avec `StoreError::ModReadOnly`, quel que soit le détour (racine imbriquée,
//!   lien, jonction, casse différente) ; ce type n'a d'ailleurs aucune méthode qui
//!   écrive, renomme ou supprime dans `MOD_ROOT` ;
//! - `CSV_ROOT` est créé s'il n'existe pas, mais jamais s'il se trouve sous `MOD_ROOT` ;
//! - un fichier existant n'est jamais écrasé (sauvegarde + confirmation : étape suivante).
//!
//! Les limites de la sandbox (hardlink, TOCTOU) s'appliquent telles quelles.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::config::Config;
use crate::sandbox::{Sandbox, SandboxError};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error("MOD_ROOT {mod_root:?} inutilisable: {reason}")]
    ModRootUnusable { mod_root: PathBuf, reason: String },
    #[error("CSV_ROOT {csv_root:?} inutilisable: {reason}")]
    CsvRootUnusable { csv_root: PathBuf, reason: String },
    #[error("CSV_ROOT {csv_root:?} refusé: il est situé sous MOD_ROOT {mod_root:?}, qui est en lecture seule. Choisir un dossier hors du mod")]
    CsvRootInsideMod {
        csv_root: PathBuf,
        mod_root: PathBuf,
    },
    #[error("écriture refusée: {path:?} est sous MOD_ROOT {mod_root:?}, qui est en lecture seule. Les CSV s'écrivent uniquement sous CSV_ROOT")]
    ModReadOnly { path: PathBuf, mod_root: PathBuf },
    #[error("écriture refusée: {path:?} existe déjà (aucun écrasement; la sauvegarde et la confirmation ne sont pas encore implémentées)")]
    AlreadyExists { path: PathBuf },
    #[error("erreur d'entrée/sortie sur {path:?}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone)]
pub struct CsvStore {
    csv: Sandbox,
    /// `MOD_ROOT` canonique, utilisé uniquement pour refuser les écritures.
    mod_root: PathBuf,
}

impl CsvStore {
    /// Ouvre le magasin : valide `MOD_ROOT` (dossier existant), refuse un `CSV_ROOT` situé
    /// sous lui, puis crée `CSV_ROOT` (et ses parents) s'il n'existe pas.
    /// Rien n'est créé si un contrôle échoue.
    pub fn open(config: &Config) -> Result<CsvStore, StoreError> {
        let mod_root = config
            .mod_root
            .canonicalize()
            .map_err(|e| StoreError::ModRootUnusable {
                mod_root: config.mod_root.clone(),
                reason: e.to_string(),
            })?;
        if !mod_root.is_dir() {
            return Err(StoreError::ModRootUnusable {
                mod_root: config.mod_root.clone(),
                reason: "ce n'est pas un dossier".into(),
            });
        }
        let planned = canonicalize_lenient(&config.csv_root)?;
        if planned.starts_with(&mod_root) {
            return Err(StoreError::CsvRootInsideMod {
                csv_root: config.csv_root.clone(),
                mod_root,
            });
        }
        std::fs::create_dir_all(&config.csv_root).map_err(|source| StoreError::Io {
            path: config.csv_root.clone(),
            source,
        })?;
        let csv = Sandbox::new(&config.csv_root)?;
        if csv.root().starts_with(&mod_root) {
            return Err(StoreError::CsvRootInsideMod {
                csv_root: config.csv_root.clone(),
                mod_root,
            });
        }
        Ok(CsvStore { csv, mod_root })
    }

    /// `CSV_ROOT` canonique.
    pub fn csv_root(&self) -> &Path {
        self.csv.root()
    }

    /// Refuse tout chemin sous `MOD_ROOT`. À appeler avant toute écriture sur un chemin
    /// déjà résolu.
    pub fn guard_write(&self, path: &Path) -> Result<(), StoreError> {
        if path.starts_with(&self.mod_root) {
            return Err(StoreError::ModReadOnly {
                path: path.to_path_buf(),
                mod_root: self.mod_root.clone(),
            });
        }
        Ok(())
    }

    /// Lit un CSV existant sous `CSV_ROOT`.
    pub fn read_csv(&self, rel: &str) -> Result<Vec<u8>, StoreError> {
        let path = self.csv.resolve_read(rel)?;
        std::fs::read(&path).map_err(|source| StoreError::Io { path, source })
    }

    /// Écrit un nouveau CSV sous `CSV_ROOT` (dossiers intermédiaires créés au besoin).
    /// Refuse un chemin qui se résout sous `MOD_ROOT` et un fichier déjà existant.
    /// Renvoie le chemin réel écrit.
    pub fn write_csv(&self, rel: &str, bytes: &[u8]) -> Result<PathBuf, StoreError> {
        let target = match self.csv.resolve_write(rel) {
            Ok(p) => p,
            // Un lien ou une jonction qui mène dans le mod : message explicite « mod ».
            Err(SandboxError::Escapes { resolved, .. }) if resolved.starts_with(&self.mod_root) => {
                return Err(StoreError::ModReadOnly {
                    path: resolved,
                    mod_root: self.mod_root.clone(),
                });
            }
            Err(e) => return Err(e.into()),
        };
        self.guard_write(&target)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::AlreadyExists {
                    StoreError::AlreadyExists {
                        path: target.clone(),
                    }
                } else {
                    StoreError::Io {
                        path: target.clone(),
                        source,
                    }
                }
            })?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| StoreError::Io {
                path: target.clone(),
                source,
            })?;
        Ok(target)
    }
}

/// Chemin canonique d'une racine qui n'existe peut-être pas encore : le plus profond
/// ancêtre existant est canonicalisé, le reste (inexistant) est ajouté tel quel. Un `..`
/// dans la partie inexistante est refusé.
fn canonicalize_lenient(path: &Path) -> Result<PathBuf, StoreError> {
    let unusable = |reason: String| StoreError::CsvRootUnusable {
        csv_root: path.to_path_buf(),
        reason,
    };
    let mut existing = std::path::absolute(path).map_err(|e| unusable(e.to_string()))?;
    let mut rest: Vec<OsString> = Vec::new();
    loop {
        match std::fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(e) => return Err(unusable(e.to_string())),
        }
        if existing.components().any(|c| c == Component::ParentDir) {
            return Err(unusable("composant '..' dans la partie à créer".into()));
        }
        match existing.file_name() {
            Some(name) => rest.push(name.to_owned()),
            None => return Err(unusable("aucun ancêtre existant".into())),
        }
        existing.pop();
    }
    let mut real = existing
        .canonicalize()
        .map_err(|e| unusable(format!("{}: {e}", existing.display())))?;
    for name in rest.into_iter().rev() {
        real.push(name);
    }
    Ok(real)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Env {
        _tmp: tempfile::TempDir,
        mod_root: PathBuf,
        csv_root: PathBuf,
    }

    /// <tmp>/mod (avec modinfo.json) et <tmp>/data/csv à créer.
    fn env() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let mod_root = tmp.path().join("mod");
        fs::create_dir_all(mod_root.join("Content")).unwrap();
        fs::write(mod_root.join("modinfo.json"), "{}").unwrap();
        Env {
            mod_root,
            csv_root: tmp.path().join("data").join("csv"),
            _tmp: tmp,
        }
    }

    fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push((p.clone(), fs::read(&p).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    fn open(e: &Env) -> CsvStore {
        CsvStore::open(&Config::new(&e.mod_root, Some(e.csv_root.clone()))).unwrap()
    }

    #[test]
    fn open_creates_csv_root_and_parents_idempotently() {
        let e = env();
        assert!(!e.csv_root.exists());
        let s = open(&e);
        assert!(e.csv_root.is_dir());
        assert_eq!(s.csv_root(), e.csv_root.canonicalize().unwrap());
        open(&e); // deuxième ouverture sans erreur
    }

    #[test]
    fn open_refuses_a_missing_or_invalid_mod_root_without_creating_anything() {
        let e = env();
        let cfg = Config::new(e.mod_root.join("absent"), Some(e.csv_root.clone()));
        assert!(matches!(
            CsvStore::open(&cfg),
            Err(StoreError::ModRootUnusable { .. })
        ));
        let cfg = Config::new(e.mod_root.join("modinfo.json"), Some(e.csv_root.clone()));
        assert!(matches!(
            CsvStore::open(&cfg),
            Err(StoreError::ModRootUnusable { .. })
        ));
        assert!(!e.csv_root.exists());
    }

    #[test]
    fn open_refuses_csv_root_under_or_equal_to_mod_root_and_creates_nothing() {
        let e = env();
        let before = tree(&e.mod_root);
        for csv_root in [
            e.mod_root.clone(),
            e.mod_root.join("Content"),
            e.mod_root.join("nouveau").join("csv"), // inexistant: ne doit pas être créé
        ] {
            let cfg = Config::new(&e.mod_root, Some(csv_root.clone()));
            let err = CsvStore::open(&cfg).unwrap_err();
            assert!(
                matches!(err, StoreError::CsvRootInsideMod { .. }),
                "{csv_root:?}"
            );
            assert!(err.to_string().contains("lecture seule"));
        }
        assert!(!e.mod_root.join("nouveau").exists());
        assert_eq!(tree(&e.mod_root), before);
    }

    #[test]
    fn open_refuses_dotdot_in_the_part_to_create() {
        let e = env();
        let sneaky = e
            .mod_root
            .join("nouveau")
            .join("..")
            .join("..")
            .join("mod")
            .join("x");
        let cfg = Config::new(&e.mod_root, Some(sneaky));
        assert!(CsvStore::open(&cfg).is_err());
        assert!(!e.mod_root.join("nouveau").exists());
    }

    #[test]
    fn write_then_read_back_creating_subfolders() {
        let e = env();
        let s = open(&e);
        let path = s
            .write_csv("datatables/DT_X.csv", b"---,A\n1,\"2\"\n")
            .unwrap();
        assert!(path.starts_with(s.csv_root()));
        assert_eq!(fs::read(&path).unwrap(), b"---,A\n1,\"2\"\n");
        assert_eq!(
            s.read_csv("datatables/DT_X.csv").unwrap(),
            b"---,A\n1,\"2\"\n"
        );
    }

    #[test]
    fn write_never_overwrites() {
        let e = env();
        let s = open(&e);
        s.write_csv("a.csv", b"v1").unwrap();
        let err = s.write_csv("a.csv", b"v2").unwrap_err();
        assert!(matches!(err, StoreError::AlreadyExists { .. }));
        assert_eq!(s.read_csv("a.csv").unwrap(), b"v1");
    }

    #[test]
    fn sandbox_rules_apply_to_writes() {
        let e = env();
        let s = open(&e);
        for rel in [
            "../mod/x.csv",
            "/abs.csv",
            "C:/x.csv",
            "a\\b.csv",
            "NUL.csv",
        ] {
            assert!(
                matches!(s.write_csv(rel, b"x"), Err(StoreError::Sandbox(_))),
                "{rel}"
            );
        }
        assert!(!e.mod_root.join("x.csv").exists());
    }

    #[test]
    fn writing_under_mod_root_is_refused_when_csv_root_is_its_parent() {
        // CSV_ROOT = <tmp>, MOD_ROOT = <tmp>/mod : "mod/..." est dans CSV_ROOT mais aussi dans le mod.
        let e = env();
        let parent = e.mod_root.parent().unwrap().to_path_buf();
        let s = CsvStore::open(&Config::new(&e.mod_root, Some(parent))).unwrap();
        let before = tree(&e.mod_root);
        for rel in [
            "mod/new.csv",
            "mod/Content/new.csv",
            "mod/nouveau/dossier/new.csv",
        ] {
            let err = s.write_csv(rel, b"x").unwrap_err();
            assert!(
                matches!(err, StoreError::ModReadOnly { .. }),
                "{rel}: {err}"
            );
            assert!(err.to_string().contains("lecture seule"));
        }
        assert!(!e.mod_root.join("nouveau").exists());
        assert_eq!(tree(&e.mod_root), before);
        // hors du mod, le même magasin écrit normalement
        assert!(s.write_csv("autre/ok.csv", b"x").is_ok());
    }

    #[test]
    fn guard_write_flags_any_path_under_mod_root() {
        let e = env();
        let s = open(&e);
        let m = e.mod_root.canonicalize().unwrap();
        assert!(matches!(
            s.guard_write(&m.join("modinfo.json")),
            Err(StoreError::ModReadOnly { .. })
        ));
        assert!(matches!(
            s.guard_write(&m),
            Err(StoreError::ModReadOnly { .. })
        ));
        assert!(s.guard_write(s.csv_root()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_from_csv_root_into_the_mod_is_refused_with_the_mod_message() {
        let e = env();
        let s = open(&e);
        std::os::unix::fs::symlink(&e.mod_root, s.csv_root().join("viamod")).unwrap();
        let before = tree(&e.mod_root);
        let err = s.write_csv("viamod/new.csv", b"x").unwrap_err();
        assert!(matches!(err, StoreError::ModReadOnly { .. }), "{err}");
        let err = s
            .write_csv("viamod/Content/deep/new.csv", b"x")
            .unwrap_err();
        assert!(matches!(err, StoreError::ModReadOnly { .. }), "{err}");
        assert_eq!(tree(&e.mod_root), before);
    }

    #[cfg(windows)]
    #[test]
    fn case_variants_and_junction_into_the_mod_are_refused() {
        let e = env();
        // casse différente du dossier du mod, CSV_ROOT parent du mod
        let parent = e.mod_root.parent().unwrap().to_path_buf();
        let s = CsvStore::open(&Config::new(&e.mod_root, Some(parent))).unwrap();
        let before = tree(&e.mod_root);
        let err = s.write_csv("MOD/new.csv", b"x").unwrap_err();
        assert!(matches!(err, StoreError::ModReadOnly { .. }), "{err}");

        // jonction NTFS depuis CSV_ROOT vers le mod
        let e2 = env();
        let s2 = open(&e2);
        let out = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(s2.csv_root().join("viamod"))
            .arg(&e2.mod_root)
            .output()
            .unwrap();
        assert!(out.status.success());
        let before2 = tree(&e2.mod_root);
        let err = s2.write_csv("viamod/new.csv", b"x").unwrap_err();
        assert!(matches!(err, StoreError::ModReadOnly { .. }), "{err}");
        assert_eq!(tree(&e2.mod_root), before2);
        assert_eq!(tree(&e.mod_root), before);
    }

    /// Tests sur le vrai disque de la machine de développement : MOD_ROOT réel (lecture
    /// seule, jamais écrit) et CSV_ROOT par défaut `D:\ConanMod\csv`. Ignorés (ligne
    /// `SKIPPED`) si le dossier du mod n'existe pas sur la machine.
    #[cfg(windows)]
    mod real {
        use super::*;
        use crate::csv::Table;
        use std::time::SystemTime;

        const REAL_MOD_ROOT: &str = r"D:\Program Files\CEUE5Devkit\UE4\Content\Mods\test";
        const FIXTURE: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/DT_LightningRodLoot_Common.csv"
        ));

        /// Contenu, taille et date de modification des fichiers du mod à surveiller.
        type Snapshot = Vec<(&'static str, Vec<u8>, u64, SystemTime)>;

        fn snapshot() -> Snapshot {
            ["modinfo.json", "active.txt"]
                .into_iter()
                .map(|name| {
                    let p = Path::new(REAL_MOD_ROOT).join(name);
                    let meta = fs::metadata(&p).unwrap();
                    (
                        name,
                        fs::read(&p).unwrap(),
                        meta.len(),
                        meta.modified().unwrap(),
                    )
                })
                .collect()
        }

        fn available(test: &str) -> bool {
            if Path::new(REAL_MOD_ROOT).join("modinfo.json").is_file() {
                return true;
            }
            eprintln!("SKIPPED {test}: dossier du mod absent sur cette machine ({REAL_MOD_ROOT})");
            false
        }

        /// Supprime le fichier de test même si une assertion échoue.
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }

        fn unique_name() -> String {
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            format!("_conan_core_test_{}_{nanos}.csv", std::process::id())
        }

        fn round_trip_on_default_csv_root() {
            let store = CsvStore::open(&Config::new(REAL_MOD_ROOT, None)).unwrap(); // crée D:\ConanMod\csv
            assert!(store.csv_root().is_dir());
            assert!(
                store
                    .csv_root()
                    .to_string_lossy()
                    .to_lowercase()
                    .ends_with(r"conanmod\csv"),
                "{:?}",
                store.csv_root()
            );
            let name = unique_name();
            let rel = name.clone();
            let expected = store.csv_root().join(&name);
            let _cleanup = Cleanup(expected.clone());

            // création + écriture
            let written = store.write_csv(&rel, FIXTURE).unwrap();
            assert_eq!(written, expected);
            assert!(written.is_file());
            assert!(store.guard_write(&written).is_ok());
            // relecture : octets identiques, et round-trip du parseur
            let back = store.read_csv(&rel).unwrap();
            assert_eq!(back, FIXTURE);
            assert_eq!(Table::parse(&back).unwrap().to_bytes(), FIXTURE);
            // pas d'écrasement
            assert!(matches!(
                store.write_csv(&rel, b"autre"),
                Err(StoreError::AlreadyExists { .. })
            ));
            // suppression du fichier de test uniquement
            fs::remove_file(&written).unwrap();
            assert!(!written.exists());
        }

        fn refusals_under_the_real_mod_root() {
            let mod_root = Path::new(REAL_MOD_ROOT);
            // CSV_ROOT = le mod lui-même ou un dossier existant du mod : refusé à l'ouverture
            // (les dossiers existent déjà, donc même un défaut de contrôle ne créerait rien).
            for csv_root in [mod_root.to_path_buf(), mod_root.join("Content")] {
                let err = CsvStore::open(&Config::new(REAL_MOD_ROOT, Some(csv_root.clone())))
                    .unwrap_err();
                assert!(
                    matches!(err, StoreError::CsvRootInsideMod { .. }),
                    "{csv_root:?}: {err}"
                );
            }
            // garde-fou sur des chemins déjà résolus (aucune écriture n'est tentée)
            let store = CsvStore::open(&Config::new(REAL_MOD_ROOT, None)).unwrap();
            let canonical = mod_root.canonicalize().unwrap();
            for p in [
                canonical.clone(),
                canonical.join("modinfo.json"),
                canonical.join("active.txt"),
                canonical.join("Content").join("nouveau.csv"),
            ] {
                let err = store.guard_write(&p).unwrap_err();
                assert!(matches!(err, StoreError::ModReadOnly { .. }), "{p:?}");
                assert!(err.to_string().contains("lecture seule"));
            }
            // les sorties de CSV_ROOT vers le mod sont arrêtées par la sandbox
            for rel in ["../test/x.csv", "..\\test\\x.csv", "D:/Program Files/x.csv"] {
                assert!(
                    matches!(store.write_csv(rel, b"x"), Err(StoreError::Sandbox(_))),
                    "{rel}"
                );
            }
        }

        #[test]
        fn real_csv_root_create_write_read_delete() {
            if !available("real_csv_root_create_write_read_delete") {
                return;
            }
            round_trip_on_default_csv_root();
        }

        #[test]
        fn real_write_under_mod_root_is_refused() {
            if !available("real_write_under_mod_root_is_refused") {
                return;
            }
            refusals_under_the_real_mod_root();
        }

        #[test]
        fn real_mod_files_are_never_modified() {
            if !available("real_mod_files_are_never_modified") {
                return;
            }
            let before = snapshot();
            round_trip_on_default_csv_root();
            refusals_under_the_real_mod_root();
            assert_eq!(snapshot(), before, "modinfo.json ou active.txt a changé");
        }
    }
}
