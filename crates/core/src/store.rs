//! Écriture des CSV sous `CSV_ROOT`, jamais sous `MOD_ROOT`.
//!
//! `CsvStore` est le seul point d'écriture de fichiers de données :
//! - tout chemin passe par la sandbox de `CSV_ROOT` (contrôle lexical, liens, jonctions) ;
//! - `MOD_ROOT` est en lecture seule : tout chemin d'écriture qui se résout sous lui est
//!   refusé avec `StoreError::ModReadOnly`, quel que soit le détour (racine imbriquée,
//!   lien, jonction, casse différente) ; ce type n'a d'ailleurs aucune méthode qui
//!   écrive, renomme ou supprime dans `MOD_ROOT` ;
//! - `CSV_ROOT` est créé s'il n'existe pas, mais jamais s'il se trouve sous `MOD_ROOT` ;
//! - un fichier existant n'est remplacé qu'avec `confirm`, après sauvegarde dans
//!   `CSV_ROOT/.backup/` ; un dry-run renvoie le diff sans rien écrire ; l'écriture est
//!   atomique (fichier temporaire puis renommage).
//!
//! Les limites de la sandbox (hardlink, TOCTOU) s'appliquent telles quelles.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::config::Config;
use crate::csv::{CsvError, Table};
use crate::diff::TableDiff;
use crate::listing::{list_files, ListError, Listing};
use crate::sandbox::{check_relative, Sandbox, SandboxError};

/// Taille maximale d'un fichier lu par `read_csv` / `ModReader::read_file`.
pub const MAX_READ_BYTES: u64 = 16 * 1024 * 1024;

/// Dossier des sauvegardes, sous `CSV_ROOT`. Jamais purgé automatiquement.
pub const BACKUP_DIR: &str = ".backup";

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
    #[error("écriture refusée: {path:?} existe déjà; pour le remplacer (après sauvegarde dans .backup), confirmer explicitement avec confirm")]
    AlreadyExists { path: PathBuf },
    #[error(
        "chemin {path:?} refusé: le dossier {BACKUP_DIR} est réservé aux sauvegardes automatiques"
    )]
    ReservedPath { path: String },
    #[error("fichier {path:?} trop gros ({size} octets, maximum {max}): lecture refusée")]
    TooLarge { path: PathBuf, size: u64, max: u64 },
    #[error(transparent)]
    List(#[from] ListError),
    #[error("contenu refusé, ce n'est pas un CSV valide: {source}")]
    InvalidCsv { source: CsvError },
    #[error("le fichier existant {path:?} n'est pas un CSV lisible ({source}): comparaison impossible, il n'est pas modifié")]
    ExistingUnreadable { path: PathBuf, source: CsvError },
    #[error("sauvegarde impossible de {path:?} ({reason}): la modification n'a pas eu lieu")]
    BackupFailed { path: PathBuf, reason: String },
    #[error("erreur d'entrée/sortie sur {path:?}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl StoreError {
    /// Nom court et stable de l'erreur, pour que les clients (MCP) puissent la reconnaître.
    pub fn code(&self) -> &'static str {
        match self {
            StoreError::Sandbox(SandboxError::NotFound { .. }) => "NotFound",
            StoreError::Sandbox(SandboxError::Escapes { .. }) => "PathEscapes",
            StoreError::Sandbox(SandboxError::WriteThroughLink { .. }) => "LinkRefused",
            StoreError::Sandbox(_) => "PathRefused",
            StoreError::ModRootUnusable { .. } => "ModRootUnusable",
            StoreError::CsvRootUnusable { .. } => "CsvRootUnusable",
            StoreError::CsvRootInsideMod { .. } => "CsvRootInsideMod",
            StoreError::ModReadOnly { .. } => "ModReadOnly",
            StoreError::AlreadyExists { .. } => "AlreadyExists",
            StoreError::ReservedPath { .. } => "ReservedPath",
            StoreError::BackupFailed { .. } => "BackupFailed",
            StoreError::TooLarge { .. } => "TooLarge",
            StoreError::List(ListError::Sandbox(SandboxError::NotFound { .. })) => "NotFound",
            StoreError::List(ListError::Sandbox(SandboxError::Escapes { .. })) => "PathEscapes",
            StoreError::List(ListError::Sandbox(_)) => "PathRefused",
            StoreError::List(ListError::InternalFolder { .. }) => "InternalFolder",
            StoreError::List(_) => "ListFailed",
            StoreError::InvalidCsv { .. } => "InvalidCsv",
            StoreError::ExistingUnreadable { .. } => "ExistingUnreadable",
            StoreError::Io { .. } => "IoError",
        }
    }
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

    /// Copie un CSV existant dans `CSV_ROOT/.backup/`, en conservant son sous-dossier, avec
    /// l'horodatage UTC dans le nom : `DT_X.csv` devient
    /// `.backup/<sous-dossier>/DT_X.20261017T221530.123Z.csv`. Le chemin de la sauvegarde
    /// passe par la sandbox comme tout autre. Ne remplace jamais une sauvegarde existante.
    /// Renvoie le chemin de la sauvegarde.
    pub fn backup_csv(&self, rel: &str) -> Result<PathBuf, StoreError> {
        let source = self.csv.resolve_read(rel)?;
        let bytes = std::fs::read(&source).map_err(|e| StoreError::BackupFailed {
            path: source.clone(),
            reason: e.to_string(),
        })?;
        self.backup_bytes(rel, &bytes)
    }

    fn backup_bytes(&self, rel: &str, original: &[u8]) -> Result<PathBuf, StoreError> {
        let failed = |path: &Path, reason: String| StoreError::BackupFailed {
            path: path.to_path_buf(),
            reason,
        };
        let stamp = format_timestamp(std::time::SystemTime::now());
        let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        let dir_prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        for attempt in 0..1000 {
            let unique = if attempt == 0 {
                stamp.clone()
            } else {
                format!("{stamp}-{attempt}")
            };
            let file = match name.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() => format!("{stem}.{unique}.{ext}"),
                _ => format!("{name}.{unique}"),
            };
            let backup_rel = format!("{BACKUP_DIR}/{dir_prefix}{file}");
            let target = self
                .csv
                .resolve_write(&backup_rel)
                .map_err(|e| failed(Path::new(&backup_rel), e.to_string()))?;
            self.guard_write(&target)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| failed(parent, e.to_string()))?;
            }
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
            {
                Ok(mut f) => {
                    if let Err(e) = f.write_all(original).and_then(|()| f.sync_all()) {
                        drop(f);
                        let _ = std::fs::remove_file(&target); // sauvegarde incomplète: pas de demi-sauvegarde
                        return Err(failed(&target, e.to_string()));
                    }
                    return Ok(target);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(failed(&target, e.to_string())),
            }
        }
        Err(failed(
            Path::new(rel),
            "trop de sauvegardes de même horodatage".into(),
        ))
    }

    /// Lit un CSV existant sous `CSV_ROOT` (taille limitée à `MAX_READ_BYTES`).
    pub fn read_csv(&self, rel: &str) -> Result<Vec<u8>, StoreError> {
        read_capped(&self.csv, rel)
    }

    /// Liste les fichiers de `CSV_ROOT` (ou d'un sous-dossier), sans `.backup` ni temporaires.
    pub fn list_files(&self, subfolder: Option<&str>) -> Result<Listing, StoreError> {
        Ok(list_files(&self.csv, subfolder)?)
    }

    /// Écrit un CSV sous `CSV_ROOT` (dossiers intermédiaires créés au besoin, sauf en
    /// dry-run). Le contenu doit être un CSV lisible par `Table::parse`.
    ///
    /// - **dry-run** : renvoie le diff sans rien écrire (ni fichier, ni dossier, ni
    ///   sauvegarde), même si le fichier existe et sans `confirm`.
    /// - **fichier nouveau** : écrit sans confirmation.
    /// - **fichier existant** : sans `confirm`, `AlreadyExists`. Avec `confirm`, le fichier
    ///   est d'abord sauvegardé dans `.backup/` (si la sauvegarde échoue, rien n'est
    ///   modifié), puis remplacé. Un contenu identique à l'existant ne change rien (pas
    ///   de sauvegarde, `unchanged: true`).
    /// - l'écriture est atomique : fichier temporaire du même dossier, puis renommage.
    ///
    /// Refuse un chemin résolu sous `MOD_ROOT` et le dossier réservé `.backup`.
    pub fn write_csv(
        &self,
        rel: &str,
        bytes: &[u8],
        options: WriteOptions,
    ) -> Result<WriteOutcome, StoreError> {
        let comps = check_relative(rel)?;
        if comps[0].eq_ignore_ascii_case(BACKUP_DIR) {
            return Err(StoreError::ReservedPath {
                path: rel.to_owned(),
            });
        }
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

        let new_table = Table::parse(bytes).map_err(|source| StoreError::InvalidCsv { source })?;
        let existing = match std::fs::read(&target) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(StoreError::Io {
                    path: target,
                    source,
                })
            }
        };
        let old_table = match &existing {
            Some(b) => Some(
                Table::parse(b).map_err(|source| StoreError::ExistingUnreadable {
                    path: target.clone(),
                    source,
                })?,
            ),
            None => None,
        };
        let mut outcome = WriteOutcome {
            path: target.clone(),
            existed: existing.is_some(),
            applied: false,
            unchanged: existing.as_deref() == Some(bytes),
            backup: None,
            diff: TableDiff::between(old_table.as_ref(), &new_table),
        };
        if options.dry_run || outcome.unchanged {
            return Ok(outcome);
        }
        if let Some(original) = &existing {
            if !options.confirm {
                return Err(StoreError::AlreadyExists { path: target });
            }
            // Sauvegarde d'abord : en cas d'échec, l'original n'est pas touché.
            outcome.backup = Some(self.backup_bytes(rel, original)?);
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        write_atomic(&target, bytes)?;
        outcome.applied = true;
        Ok(outcome)
    }
}

/// Options d'une écriture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteOptions {
    /// Ne rien écrire, renvoyer seulement le diff.
    pub dry_run: bool,
    /// Autorise le remplacement d'un fichier existant (après sauvegarde).
    pub confirm: bool,
}

/// Résultat d'une écriture (ou d'un dry-run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    /// Chemin réel du fichier visé.
    pub path: PathBuf,
    /// Le fichier existait avant.
    pub existed: bool,
    /// Le fichier a été écrit.
    pub applied: bool,
    /// Le contenu est identique, au byte près, à l'existant : rien n'est fait.
    pub unchanged: bool,
    /// Sauvegarde créée avant le remplacement.
    pub backup: Option<PathBuf>,
    pub diff: TableDiff,
}

/// Écrit `bytes` dans un fichier temporaire du même dossier, puis le renomme sur `target` :
/// jamais de fichier à moitié écrit. Le temporaire est supprimé si quelque chose échoue.
/// (Un plantage du processus entre les deux peut laisser un `.<nom>.tmp-…` orphelin.)
fn write_atomic(target: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let io = |source| StoreError::Io {
        path: target.to_path_buf(),
        source,
    };
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return Err(io(std::io::Error::other("chemin sans dossier ou sans nom")));
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut tmp_name = OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".tmp-{}-{nanos}", std::process::id()));
    let tmp = dir.join(tmp_name);
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, target)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(io(e));
    }
    Ok(())
}

fn read_capped(sandbox: &Sandbox, rel: &str) -> Result<Vec<u8>, StoreError> {
    let path = sandbox.resolve_read(rel)?;
    let io = |source| StoreError::Io {
        path: path.clone(),
        source,
    };
    let size = std::fs::metadata(&path).map_err(io)?.len();
    if size > MAX_READ_BYTES {
        return Err(StoreError::TooLarge {
            path,
            size,
            max: MAX_READ_BYTES,
        });
    }
    std::fs::read(&path).map_err(io)
}

/// Accès en **lecture seule** au dossier du mod (`MOD_ROOT`). Ce type n'a aucune méthode qui
/// écrive, renomme ou supprime : la lecture seule est garantie par construction.
#[derive(Debug, Clone)]
pub struct ModReader {
    sandbox: Sandbox,
}

impl ModReader {
    /// `MOD_ROOT` doit être un dossier existant. Rien n'est créé.
    pub fn open(mod_root: &Path) -> Result<ModReader, StoreError> {
        let sandbox = Sandbox::new(mod_root).map_err(|e| StoreError::ModRootUnusable {
            mod_root: mod_root.to_path_buf(),
            reason: e.to_string(),
        })?;
        Ok(ModReader { sandbox })
    }

    pub fn root(&self) -> &Path {
        self.sandbox.root()
    }

    /// Liste les fichiers du mod (ou d'un sous-dossier), sans `.backup` ni temporaires.
    pub fn list_files(&self, subfolder: Option<&str>) -> Result<Listing, StoreError> {
        Ok(list_files(&self.sandbox, subfolder)?)
    }

    /// Lit un fichier du mod (taille limitée à `MAX_READ_BYTES`).
    pub fn read_file(&self, rel: &str) -> Result<Vec<u8>, StoreError> {
        read_capped(&self.sandbox, rel)
    }
}

/// Horodatage UTC pour les noms de sauvegarde : `YYYYMMDDTHHMMSS.mmmZ` (sans `:`, valide
/// dans un nom de fichier Windows).
fn format_timestamp(t: std::time::SystemTime) -> String {
    let since = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = i64::try_from(since.as_secs()).unwrap_or(i64::MAX);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Jours depuis 1970-01-01 -> date civile (algorithme de H. Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}.{:03}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60,
        since.subsec_millis()
    )
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
            .write_csv(
                "datatables/DT_X.csv",
                b"---,A\n1,\"2\"\n",
                WriteOptions::default(),
            )
            .unwrap()
            .path;
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
        s.write_csv("a.csv", b"v1", WriteOptions::default())
            .unwrap();
        let err = s
            .write_csv("a.csv", b"v2", WriteOptions::default())
            .unwrap_err();
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
                matches!(
                    s.write_csv(rel, b"x", WriteOptions::default()),
                    Err(StoreError::Sandbox(_))
                ),
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
            let err = s.write_csv(rel, b"x", WriteOptions::default()).unwrap_err();
            assert!(
                matches!(err, StoreError::ModReadOnly { .. }),
                "{rel}: {err}"
            );
            assert!(err.to_string().contains("lecture seule"));
        }
        assert!(!e.mod_root.join("nouveau").exists());
        assert_eq!(tree(&e.mod_root), before);
        // hors du mod, le même magasin écrit normalement
        assert!(s
            .write_csv("autre/ok.csv", b"x", WriteOptions::default())
            .is_ok());
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

    #[test]
    fn mod_reader_lists_and_reads_the_mod_without_any_write_api() {
        let e = env();
        let r = ModReader::open(&e.mod_root).unwrap();
        let l = r.list_files(None).unwrap();
        let paths: Vec<_> = l.entries.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["modinfo.json"]);
        assert_eq!(r.read_file("modinfo.json").unwrap(), b"{}");
        assert!(matches!(
            r.read_file("../x"),
            Err(StoreError::Sandbox(SandboxError::InvalidPath { .. }))
        ));
        assert!(matches!(
            ModReader::open(&e.mod_root.join("absent")),
            Err(StoreError::ModRootUnusable { .. })
        ));
    }

    #[test]
    fn store_listing_hides_backups_and_temporaries() {
        let e = env();
        let s = open(&e);
        s.write_csv("a.csv", b"---,A\n1,\"1\"\n", WriteOptions::default())
            .unwrap();
        s.write_csv("a.csv", b"---,A\n1,\"2\"\n", confirmed())
            .unwrap();
        fs::write(s.csv_root().join(".a.csv.tmp-1-2"), "x").unwrap();
        let l = s.list_files(None).unwrap();
        let paths: Vec<_> = l.entries.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a.csv"]);
    }

    #[test]
    fn oversized_files_are_not_read() {
        let e = env();
        let s = open(&e);
        let big = fs::File::create(s.csv_root().join("big.csv")).unwrap();
        big.set_len(MAX_READ_BYTES + 1).unwrap();
        assert!(matches!(
            s.read_csv("big.csv"),
            Err(StoreError::TooLarge { .. })
        ));
    }

    #[test]
    fn timestamp_format() {
        use std::time::{Duration, UNIX_EPOCH};
        assert_eq!(format_timestamp(UNIX_EPOCH), "19700101T000000.000Z");
        let t = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        assert_eq!(format_timestamp(t), "20231114T221320.123Z");
        let leap = UNIX_EPOCH + Duration::from_secs(1_709_164_800); // 2024-02-29
        assert_eq!(format_timestamp(leap), "20240229T000000.000Z");
    }

    #[test]
    fn backup_is_an_identical_timestamped_copy_under_csv_root_backup_dir() {
        let e = env();
        let s = open(&e);
        s.write_csv(
            "datatables/DT_X.csv",
            b"---,A\n1,\"2\"\n",
            WriteOptions::default(),
        )
        .unwrap();
        let b = s.backup_csv("datatables/DT_X.csv").unwrap();
        assert!(b.starts_with(s.csv_root().join(BACKUP_DIR).join("datatables")));
        let name = b.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("DT_X.") && name.ends_with("Z.csv"),
            "{name}"
        );
        assert_eq!(fs::read(&b).unwrap(), b"---,A\n1,\"2\"\n");
        // l'original est intact et une 2e sauvegarde ne remplace pas la 1re
        assert_eq!(
            s.read_csv("datatables/DT_X.csv").unwrap(),
            b"---,A\n1,\"2\"\n"
        );
        let b2 = s.backup_csv("datatables/DT_X.csv").unwrap();
        assert_ne!(b, b2);
        assert!(b.exists() && b2.exists());
    }

    #[test]
    fn backup_of_a_missing_file_or_without_extension() {
        let e = env();
        let s = open(&e);
        assert!(matches!(
            s.backup_csv("absent.csv"),
            Err(StoreError::Sandbox(SandboxError::NotFound { .. }))
        ));
        s.write_csv("README", b"---\n", WriteOptions::default())
            .unwrap();
        let b = s.backup_csv("README").unwrap();
        assert!(b
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("README."));
    }

    #[test]
    fn backup_dir_is_reserved_for_writes() {
        let e = env();
        let s = open(&e);
        for rel in [".backup/x.csv", ".BACKUP/x.csv", ".backup/sub/x.csv"] {
            assert!(
                matches!(
                    s.write_csv(rel, b"---\n", WriteOptions::default()),
                    Err(StoreError::ReservedPath { .. })
                ),
                "{rel}"
            );
        }
        assert!(!s.csv_root().join(BACKUP_DIR).exists());
    }

    #[test]
    fn backup_failure_is_reported_and_leaves_the_original_alone() {
        let e = env();
        let s = open(&e);
        s.write_csv("a.csv", b"---,A\n1,\"1\"\n", WriteOptions::default())
            .unwrap();
        // `.backup` existe mais est un fichier: impossible d'y créer une sauvegarde
        fs::write(s.csv_root().join(BACKUP_DIR), "obstacle").unwrap();
        let err = s.backup_csv("a.csv").unwrap_err();
        assert!(matches!(err, StoreError::BackupFailed { .. }), "{err}");
        assert!(err.to_string().contains("n'a pas eu lieu"));
        assert_eq!(s.read_csv("a.csv").unwrap(), b"---,A\n1,\"1\"\n");
    }

    // --- Sauvegarde, dry-run, confirm, écriture atomique ---------------------------

    const V1: &[u8] = b"---,A\n1,\"x\"\n2,\"y\"\n";
    const V2: &[u8] = b"---,A\n1,\"x\"\n2,\"Y\"\n3,\"z\"\n";

    fn confirmed() -> WriteOptions {
        WriteOptions {
            confirm: true,
            ..WriteOptions::default()
        }
    }

    fn dry(confirm: bool) -> WriteOptions {
        WriteOptions {
            dry_run: true,
            confirm,
        }
    }

    /// Fichiers ET dossiers sous `dir`, avec taille et date de modification.
    fn listing(dir: &Path) -> Vec<(PathBuf, bool, u64, Option<std::time::SystemTime>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                let m = fs::metadata(&p).unwrap();
                if m.is_dir() {
                    stack.push(p.clone());
                }
                out.push((p, m.is_dir(), m.len(), m.modified().ok()));
            }
        }
        out.sort();
        out
    }

    fn leftover_temp_files(dir: &Path) -> Vec<PathBuf> {
        listing(dir)
            .into_iter()
            .map(|(p, ..)| p)
            .filter(|p| p.to_string_lossy().contains(".tmp-"))
            .collect()
    }

    #[test]
    fn dry_run_changes_nothing_and_creates_no_backup() {
        let e = env();
        let s = open(&e);
        s.write_csv("a.csv", V1, WriteOptions::default()).unwrap();
        let before = listing(s.csv_root());
        let before_mod = tree(&e.mod_root);
        for confirm in [false, true] {
            let out = s.write_csv("a.csv", V2, dry(confirm)).unwrap();
            assert!(out.existed && !out.applied && !out.unchanged && out.backup.is_none());
            assert_eq!(out.diff.added.len(), 1);
            assert_eq!(out.diff.modified.len(), 1);
            assert!(out.diff.removed.is_empty());
            assert_eq!(out.diff.added[0].key, "3");
            assert_eq!(out.diff.modified[0].key, "2");
            assert_eq!(listing(s.csv_root()), before, "confirm={confirm}");
        }
        assert_eq!(s.read_csv("a.csv").unwrap(), V1);
        assert!(!s.csv_root().join(BACKUP_DIR).exists());
        assert_eq!(tree(&e.mod_root), before_mod);
    }

    #[test]
    fn dry_run_on_a_new_file_creates_neither_file_nor_folders() {
        let e = env();
        let s = open(&e);
        let before = listing(s.csv_root());
        let out = s
            .write_csv("nouveau/dossier/b.csv", V1, dry(false))
            .unwrap();
        assert!(!out.existed && !out.applied);
        assert_eq!(out.diff.added.len(), 2);
        assert_eq!(listing(s.csv_root()), before);
        assert!(!s.csv_root().join("nouveau").exists());
    }

    #[test]
    fn dry_run_still_applies_every_refusal() {
        let e = env();
        let s = open(&e);
        assert!(matches!(
            s.write_csv("../x.csv", V1, dry(false)),
            Err(StoreError::Sandbox(_))
        ));
        assert!(matches!(
            s.write_csv(".backup/x.csv", V1, dry(false)),
            Err(StoreError::ReservedPath { .. })
        ));
        assert!(matches!(
            s.write_csv("x.csv", b"\"non ferme", dry(false)),
            Err(StoreError::InvalidCsv { .. })
        ));
    }

    #[test]
    fn overwrite_without_confirm_is_refused_and_leaves_everything_alone() {
        let e = env();
        let s = open(&e);
        s.write_csv("a.csv", V1, WriteOptions::default()).unwrap();
        let before = listing(s.csv_root());
        let err = s
            .write_csv("a.csv", V2, WriteOptions::default())
            .unwrap_err();
        assert!(matches!(err, StoreError::AlreadyExists { .. }));
        assert!(err.to_string().contains("confirm"));
        assert_eq!(listing(s.csv_root()), before);
        assert_eq!(s.read_csv("a.csv").unwrap(), V1);
    }

    #[test]
    fn overwrite_with_confirm_backs_up_then_writes_atomically() {
        let e = env();
        let s = open(&e);
        s.write_csv("data/a.csv", V1, WriteOptions::default())
            .unwrap();
        let out = s.write_csv("data/a.csv", V2, confirmed()).unwrap();
        assert!(out.existed && out.applied && !out.unchanged);
        let backup = out.backup.clone().expect("sauvegarde attendue");
        assert!(backup.starts_with(s.csv_root().join(BACKUP_DIR).join("data")));
        assert_eq!(
            fs::read(&backup).unwrap(),
            V1,
            "sauvegarde identique à l'original"
        );
        assert_eq!(s.read_csv("data/a.csv").unwrap(), V2, "nouveau contenu");
        assert_eq!(
            out.diff.summary(),
            "1 ajoutée(s), 1 modifiée(s), 0 supprimée(s)"
        );
        assert!(leftover_temp_files(s.csv_root()).is_empty());
        // le fichier parse et revalide: le nouveau contenu est exactement V2
        assert_eq!(
            Table::parse(&s.read_csv("data/a.csv").unwrap())
                .unwrap()
                .to_bytes(),
            V2
        );
    }

    #[test]
    fn successive_overwrites_keep_every_backup() {
        let e = env();
        let s = open(&e);
        s.write_csv("a.csv", V1, WriteOptions::default()).unwrap();
        let b1 = s
            .write_csv("a.csv", V2, confirmed())
            .unwrap()
            .backup
            .unwrap();
        let b2 = s
            .write_csv("a.csv", V1, confirmed())
            .unwrap()
            .backup
            .unwrap();
        assert_ne!(b1, b2);
        assert_eq!(fs::read(&b1).unwrap(), V1);
        assert_eq!(fs::read(&b2).unwrap(), V2);
    }

    #[test]
    fn identical_content_is_a_no_op_without_backup() {
        let e = env();
        let s = open(&e);
        s.write_csv("a.csv", V1, WriteOptions::default()).unwrap();
        for opts in [WriteOptions::default(), confirmed()] {
            let out = s.write_csv("a.csv", V1, opts).unwrap();
            assert!(out.unchanged && !out.applied && out.backup.is_none());
            assert!(out.diff.is_empty());
        }
        assert!(!s.csv_root().join(BACKUP_DIR).exists());
    }

    #[test]
    fn row_reordering_is_a_real_change_that_gets_backed_up() {
        let e = env();
        let s = open(&e);
        s.write_csv("a.csv", V1, WriteOptions::default()).unwrap();
        let reordered = b"---,A\n2,\"y\"\n1,\"x\"\n";
        let out = s.write_csv("a.csv", reordered, confirmed()).unwrap();
        assert!(out.diff.order_changed && out.applied && out.backup.is_some());
    }

    #[test]
    fn simulated_backup_failure_leaves_the_original_intact() {
        let e = env();
        let s = open(&e);
        s.write_csv("a.csv", V1, WriteOptions::default()).unwrap();
        // `.backup` est un fichier: la sauvegarde est impossible
        fs::write(s.csv_root().join(BACKUP_DIR), "obstacle").unwrap();
        let before = listing(s.csv_root());
        let err = s.write_csv("a.csv", V2, confirmed()).unwrap_err();
        assert!(matches!(err, StoreError::BackupFailed { .. }), "{err}");
        assert!(err.to_string().contains("n'a pas eu lieu"));
        assert_eq!(s.read_csv("a.csv").unwrap(), V1);
        assert_eq!(
            listing(s.csv_root()),
            before,
            "aucune écriture, aucun temporaire"
        );
    }

    /// Lien de dossier: symlink (Unix) ou jonction NTFS (Windows, sans privilège).
    fn dir_link(link: &Path, target: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        {
            let out = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(out.status.success());
        }
    }

    #[test]
    fn backup_dir_is_inside_csv_root_and_goes_through_the_sandbox() {
        let e = env();
        let s = open(&e);
        s.write_csv("a.csv", V1, WriteOptions::default()).unwrap();
        // `.backup` détourné vers l'extérieur de CSV_ROOT (lien ou jonction)
        let outside = e.mod_root.parent().unwrap().join("outside");
        fs::create_dir_all(&outside).unwrap();
        dir_link(&s.csv_root().join(BACKUP_DIR), &outside);
        let err = s.write_csv("a.csv", V2, confirmed()).unwrap_err();
        assert!(matches!(err, StoreError::BackupFailed { .. }), "{err}");
        assert_eq!(s.read_csv("a.csv").unwrap(), V1, "original intact");
        assert!(
            fs::read_dir(&outside).unwrap().next().is_none(),
            "rien écrit hors CSV_ROOT"
        );
        // et, par le chemin normal, la sauvegarde est bien sous CSV_ROOT/.backup
        let e2 = env();
        let s2 = open(&e2);
        s2.write_csv("a.csv", V1, WriteOptions::default()).unwrap();
        let b = s2
            .write_csv("a.csv", V2, confirmed())
            .unwrap()
            .backup
            .unwrap();
        assert!(b.starts_with(s2.csv_root().join(BACKUP_DIR)));
    }

    #[test]
    fn backup_dir_pointing_into_the_mod_is_refused() {
        let e = env();
        let s = open(&e);
        s.write_csv("a.csv", V1, WriteOptions::default()).unwrap();
        dir_link(&s.csv_root().join(BACKUP_DIR), &e.mod_root);
        let before = tree(&e.mod_root);
        let err = s.write_csv("a.csv", V2, confirmed()).unwrap_err();
        assert!(matches!(err, StoreError::BackupFailed { .. }), "{err}");
        assert_eq!(tree(&e.mod_root), before);
        assert_eq!(s.read_csv("a.csv").unwrap(), V1);
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_temporary() {
        let e = env();
        let dir = e.csv_root.clone();
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("t.csv");
        write_atomic(&target, b"un").unwrap();
        write_atomic(&target, b"deux").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"deux");
        assert!(leftover_temp_files(&dir).is_empty());
    }

    #[test]
    fn write_atomic_failure_cleans_up_and_never_half_writes() {
        let e = env();
        let dir = e.csv_root.clone();
        fs::create_dir_all(&dir).unwrap();
        // la cible est un dossier non vide: le renommage final échoue
        let target = dir.join("t.csv");
        fs::create_dir_all(target.join("contenu")).unwrap();
        fs::write(target.join("contenu").join("f"), "x").unwrap();
        assert!(write_atomic(&target, b"nouveau").is_err());
        assert!(leftover_temp_files(&dir).is_empty());
        assert_eq!(fs::read(target.join("contenu").join("f")).unwrap(), b"x");
    }

    #[test]
    fn invalid_or_unreadable_csv_is_refused_before_any_change() {
        let e = env();
        let s = open(&e);
        s.write_csv("a.csv", V1, WriteOptions::default()).unwrap();
        let err = s
            .write_csv("a.csv", b"---,A\n1\n", confirmed())
            .unwrap_err();
        assert!(matches!(err, StoreError::InvalidCsv { .. }), "{err}");
        // fichier existant illisible (écrit hors du magasin): intact, pas de sauvegarde
        fs::write(s.csv_root().join("bad.csv"), "\"jamais ferme").unwrap();
        let err = s.write_csv("bad.csv", V1, confirmed()).unwrap_err();
        assert!(
            matches!(err, StoreError::ExistingUnreadable { .. }),
            "{err}"
        );
        assert_eq!(
            fs::read(s.csv_root().join("bad.csv")).unwrap(),
            b"\"jamais ferme"
        );
        assert!(!s.csv_root().join(BACKUP_DIR).exists());
        assert_eq!(s.read_csv("a.csv").unwrap(), V1);
    }

    #[test]
    fn overwrite_flows_never_touch_the_mod() {
        let e = env();
        let s = open(&e);
        let before = tree(&e.mod_root);
        s.write_csv("a.csv", V1, WriteOptions::default()).unwrap();
        s.write_csv("a.csv", V2, dry(true)).unwrap();
        s.write_csv("a.csv", V2, confirmed()).unwrap();
        assert_eq!(tree(&e.mod_root), before);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_from_csv_root_into_the_mod_is_refused_with_the_mod_message() {
        let e = env();
        let s = open(&e);
        std::os::unix::fs::symlink(&e.mod_root, s.csv_root().join("viamod")).unwrap();
        let before = tree(&e.mod_root);
        let err = s
            .write_csv("viamod/new.csv", b"x", WriteOptions::default())
            .unwrap_err();
        assert!(matches!(err, StoreError::ModReadOnly { .. }), "{err}");
        let err = s
            .write_csv("viamod/Content/deep/new.csv", b"x", WriteOptions::default())
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
        let err = s
            .write_csv("MOD/new.csv", b"x", WriteOptions::default())
            .unwrap_err();
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
        let err = s2
            .write_csv("viamod/new.csv", b"x", WriteOptions::default())
            .unwrap_err();
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
            let mut extended = FIXTURE.to_vec();
            extended.extend_from_slice(b"5,\"12000\",\"1\",\"3\"\n");
            let _cleanup = Cleanup(expected.clone());

            // création + écriture
            let written = store
                .write_csv(&rel, FIXTURE, WriteOptions::default())
                .unwrap()
                .path;
            assert_eq!(written, expected);
            assert!(written.is_file());
            assert!(store.guard_write(&written).is_ok());
            // relecture : octets identiques, et round-trip du parseur
            let back = store.read_csv(&rel).unwrap();
            assert_eq!(back, FIXTURE);
            assert_eq!(Table::parse(&back).unwrap().to_bytes(), FIXTURE);
            // pas d'écrasement sans confirm
            assert!(matches!(
                store.write_csv(&rel, &extended, WriteOptions::default()),
                Err(StoreError::AlreadyExists { .. })
            ));
            // dry-run: rien ne change (contenu, taille, date), pas de sauvegarde
            let meta = fs::metadata(&written).unwrap();
            let dry = store
                .write_csv(
                    &rel,
                    &extended,
                    WriteOptions {
                        dry_run: true,
                        confirm: true,
                    },
                )
                .unwrap();
            assert!(!dry.applied && dry.backup.is_none() && dry.diff.added.len() == 1);
            let meta2 = fs::metadata(&written).unwrap();
            assert_eq!(
                (meta.len(), meta.modified().unwrap()),
                (meta2.len(), meta2.modified().unwrap())
            );
            assert_eq!(fs::read(&written).unwrap(), FIXTURE);
            // modification confirmée: sauvegarde sous CSV_ROOT\.backup, identique à l'original
            let out = store
                .write_csv(
                    &rel,
                    &extended,
                    WriteOptions {
                        dry_run: false,
                        confirm: true,
                    },
                )
                .unwrap();
            assert!(out.applied);
            let backup = out.backup.unwrap();
            let _backup_cleanup = Cleanup(backup.clone());
            assert!(backup.starts_with(store.csv_root().join(BACKUP_DIR)));
            assert_eq!(fs::read(&backup).unwrap(), FIXTURE);
            assert_eq!(fs::read(&written).unwrap(), extended);
            assert!(store.guard_write(&backup).is_ok());
            fs::remove_file(&backup).unwrap();
            let _ = fs::remove_dir(store.csv_root().join(BACKUP_DIR)); // seulement si vide
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
                    matches!(
                        store.write_csv(rel, b"x", WriteOptions::default()),
                        Err(StoreError::Sandbox(_))
                    ),
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
