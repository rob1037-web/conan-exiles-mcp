//! Sandbox de chemins : le serveur ne lit et n'écrit que sous `MOD_ROOT`.
//!
//! Deux couches :
//!
//! 1. **Lexicale** (`check_relative`), indépendante de la plateforme. Le client envoie un
//!    chemin *relatif* en texte, avec `/` pour seul séparateur. Tout ce qui pourrait
//!    s'interpréter autrement sous Windows est refusé partout, y compris sous Linux :
//!    `\`, `:` (lettres de lecteur, flux NTFS), préfixes UNC/verbatim, `..`, noms
//!    réservés (`CON`, `NUL`…), caractères interdits, point ou espace final.
//! 2. **Système de fichiers** (`Sandbox`), qui résout le chemin réel (liens symboliques,
//!    jonctions NTFS) et vérifie qu'il reste sous la racine. Les chemins sont comparés
//!    après `canonicalize`, composant par composant : la casse est celle du disque (Windows
//!    est insensible à la casse), `\\?\` et les noms courts 8.3 sont normalisés par l'OS.
//!
//! # Limites connues (Phase 1, non traitées)
//! - **Lien physique (hardlink)** : un fichier de `MOD_ROOT` qui est un lien physique vers un
//!   fichier extérieur n'est pas détectable par son chemin ; l'écrire modifie le fichier
//!   extérieur.
//! - **TOCTOU** : entre la vérification (`resolve_*`) et l'ouverture du fichier, un autre
//!   processus peut remplacer un dossier par un lien. La résolution doit donc être faite
//!   juste avant l'écriture ; il n'y a pas de garantie absolue.

use std::fmt;
use std::path::{Path, PathBuf};

/// Règle lexicale violée par un chemin relatif.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRule {
    Empty,
    Absolute,
    Colon,
    Backslash,
    EmptyComponent,
    CurrentDir,
    ParentDir,
    ControlChar,
    ForbiddenChar,
    TrailingDotOrSpace,
    ReservedName,
}

impl fmt::Display for PathRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PathRule::Empty => "chemin vide",
            PathRule::Absolute => "chemin absolu (commence par '/'; couvre aussi UNC `//srv` et `//?/`)",
            PathRule::Colon => "caractère ':' (lettre de lecteur, `C:foo` ou flux NTFS)",
            PathRule::Backslash => "antislash '\\' (le séparateur est '/')",
            PathRule::EmptyComponent => "composant vide (`a//b` ou '/' final)",
            PathRule::CurrentDir => "composant '.'",
            PathRule::ParentDir => "composant '..'",
            PathRule::ControlChar => "caractère de contrôle",
            PathRule::ForbiddenChar => "caractère interdit sous Windows (< > \" | ? *)",
            PathRule::TrailingDotOrSpace => "composant finissant par '.' ou ' ' (Windows le tronque en silence)",
            PathRule::ReservedName => "nom de périphérique réservé sous Windows (CON, PRN, AUX, NUL, COM1-9, LPT1-9, CONIN$, CONOUT$)",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("chemin {path:?} refusé: {rule}. Attendu: chemin relatif à MOD_ROOT, composants séparés par '/', sans '..'")]
    InvalidPath { path: String, rule: PathRule },
    #[error("chemin {path:?} refusé: résolu en {resolved:?}, en dehors de MOD_ROOT ({root:?}) (lien symbolique ou jonction)")]
    Escapes {
        path: String,
        resolved: PathBuf,
        root: PathBuf,
    },
    #[error("chemin {path:?} introuvable sous MOD_ROOT ({root:?})")]
    NotFound { path: String, root: PathBuf },
    #[error("chemin {path:?} refusé: {link:?} est un lien (symbolique, jonction ou lien pendant); l'écriture à travers un lien est interdite")]
    WriteThroughLink { path: String, link: PathBuf },
    #[error("chemin {path:?} refusé: {parent:?} existe mais n'est pas un dossier")]
    ParentNotDirectory { path: String, parent: PathBuf },
    #[error("MOD_ROOT {root:?} inutilisable: {reason}")]
    BadRoot { root: PathBuf, reason: String },
    #[error("chemin {path:?}: erreur d'entrée/sortie sur {at:?}: {source}")]
    Io {
        path: String,
        at: PathBuf,
        source: std::io::Error,
    },
}

const RESERVED: &[&str] = &[
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "CONIN$",
    "CONOUT$",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "COM\u{b9}",
    "COM\u{b2}",
    "COM\u{b3}",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    "LPT\u{b9}",
    "LPT\u{b2}",
    "LPT\u{b3}",
];

/// Vérifie un chemin relatif reçu du client, sans toucher au disque.
/// Renvoie ses composants.
pub fn check_relative(path: &str) -> Result<Vec<&str>, SandboxError> {
    let bad = |rule| SandboxError::InvalidPath {
        path: path.to_owned(),
        rule,
    };
    if path.is_empty() {
        return Err(bad(PathRule::Empty));
    }
    if path.starts_with('/') {
        return Err(bad(PathRule::Absolute));
    }
    if path.contains(':') {
        return Err(bad(PathRule::Colon));
    }
    if path.contains('\\') {
        return Err(bad(PathRule::Backslash));
    }
    let mut components = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" => return Err(bad(PathRule::EmptyComponent)),
            "." => return Err(bad(PathRule::CurrentDir)),
            ".." => return Err(bad(PathRule::ParentDir)),
            _ => {}
        }
        if comp.chars().any(|c| c.is_control()) {
            return Err(bad(PathRule::ControlChar));
        }
        if comp.contains(['<', '>', '"', '|', '?', '*']) {
            return Err(bad(PathRule::ForbiddenChar));
        }
        if comp.ends_with('.') || comp.ends_with(' ') {
            return Err(bad(PathRule::TrailingDotOrSpace));
        }
        let stem = comp.split('.').next().unwrap_or(comp).trim_end_matches(' ');
        if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
            return Err(bad(PathRule::ReservedName));
        }
        components.push(comp);
    }
    Ok(components)
}

/// Racine `MOD_ROOT` canonicalisée, à travers laquelle passent tous les accès disque.
#[derive(Debug, Clone)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// `root` doit être un dossier existant (chemin OS : lettre de lecteur, UNC, `/`
    /// ou `\`, casse quelconque sous Windows). Il est canonicalisé une fois pour toutes.
    pub fn new(root: &Path) -> Result<Sandbox, SandboxError> {
        let canonical = root.canonicalize().map_err(|e| SandboxError::BadRoot {
            root: root.to_path_buf(),
            reason: e.to_string(),
        })?;
        if !canonical.is_dir() {
            return Err(SandboxError::BadRoot {
                root: root.to_path_buf(),
                reason: "ce n'est pas un dossier".into(),
            });
        }
        Ok(Sandbox { root: canonical })
    }

    /// Racine canonique.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Chemin réel d'un fichier ou dossier **existant**, garanti sous la racine.
    pub fn resolve_read(&self, rel: &str) -> Result<PathBuf, SandboxError> {
        let comps = check_relative(rel)?;
        let joined = comps.iter().fold(self.root.clone(), |p, c| p.join(c));
        match joined.canonicalize() {
            Ok(real) => self.ensure_inside(rel, real),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Un lien pendant se présente aussi comme « introuvable » : on le distingue.
                if std::fs::symlink_metadata(&joined).is_ok() {
                    return Err(SandboxError::WriteThroughLink {
                        path: rel.to_owned(),
                        link: joined,
                    });
                }
                Err(SandboxError::NotFound {
                    path: rel.to_owned(),
                    root: self.root.clone(),
                })
            }
            Err(source) => Err(SandboxError::Io {
                path: rel.to_owned(),
                at: joined,
                source,
            }),
        }
    }

    /// Chemin réel où écrire : le fichier peut ne pas exister encore, ses dossiers non
    /// plus. Le plus profond ancêtre existant est canonicalisé et doit être sous la racine.
    /// Un fichier existant qui est lui-même un lien est refusé.
    pub fn resolve_write(&self, rel: &str) -> Result<PathBuf, SandboxError> {
        let comps = check_relative(rel)?;
        let io_err = |at: &Path, source| SandboxError::Io {
            path: rel.to_owned(),
            at: at.to_path_buf(),
            source,
        };
        // Plus profond préfixe dont l'entrée existe (lien pendant compris).
        for depth in (0..=comps.len()).rev() {
            let candidate = comps[..depth]
                .iter()
                .fold(self.root.clone(), |p, c| p.join(c));
            let meta = match std::fs::symlink_metadata(&candidate) {
                Ok(m) => m,
                // NotADirectory: un composant intermédiaire est un fichier (Unix) ; on remonte
                // jusqu'à lui pour le signaler comme parent invalide.
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue
                }
                Err(e) => return Err(io_err(&candidate, e)),
            };
            let is_target = depth == comps.len();
            if meta.file_type().is_symlink() && is_target {
                return Err(SandboxError::WriteThroughLink {
                    path: rel.to_owned(),
                    link: candidate,
                });
            }
            let real = match candidate.canonicalize() {
                Ok(r) => r,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // lien pendant sur le trajet
                    return Err(SandboxError::WriteThroughLink {
                        path: rel.to_owned(),
                        link: candidate,
                    });
                }
                Err(e) => return Err(io_err(&candidate, e)),
            };
            let real = self.ensure_inside(rel, real)?;
            if !is_target && !real.is_dir() {
                return Err(SandboxError::ParentNotDirectory {
                    path: rel.to_owned(),
                    parent: real,
                });
            }
            return Ok(comps[depth..].iter().fold(real, |p, c| p.join(c)));
        }
        // La racine existe toujours ; on n'arrive ici que si elle a disparu entre-temps.
        Err(SandboxError::NotFound {
            path: rel.to_owned(),
            root: self.root.clone(),
        })
    }

    fn ensure_inside(&self, rel: &str, real: PathBuf) -> Result<PathBuf, SandboxError> {
        if real.starts_with(&self.root) {
            Ok(real)
        } else {
            Err(SandboxError::Escapes {
                path: rel.to_owned(),
                resolved: real,
                root: self.root.clone(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn rule_of(path: &str) -> PathRule {
        match check_relative(path) {
            Err(SandboxError::InvalidPath { rule, .. }) => rule,
            other => panic!("{path:?} aurait dû être refusé, reçu {other:?}"),
        }
    }

    #[test]
    fn accepts_plain_relative_paths() {
        assert_eq!(
            check_relative("datatables/DT_Loot.csv").unwrap(),
            ["datatables", "DT_Loot.csv"]
        );
        assert!(check_relative("a b/c.d.e").is_ok());
        assert!(check_relative("CONSOLE/COM10.csv").is_ok()); // pas des noms réservés
        assert!(check_relative("épée/⚔.csv").is_ok());
    }

    #[test]
    fn rejects_everything_that_could_escape_or_be_reinterpreted() {
        let cases = [
            ("", PathRule::Empty),
            ("/etc/passwd", PathRule::Absolute),
            ("//srv/share/x", PathRule::Absolute),
            ("//?/C:/x", PathRule::Absolute),
            ("C:/x", PathRule::Colon),
            ("C:x", PathRule::Colon),
            ("a/b.csv:stream", PathRule::Colon),
            ("a\\b", PathRule::Backslash),
            ("..\\x", PathRule::Backslash),
            ("a//b", PathRule::EmptyComponent),
            ("a/", PathRule::EmptyComponent),
            ("./a", PathRule::CurrentDir),
            ("a/./b", PathRule::CurrentDir),
            ("..", PathRule::ParentDir),
            ("../x", PathRule::ParentDir),
            ("a/../../x", PathRule::ParentDir),
            ("a/\u{0}b", PathRule::ControlChar),
            ("a/b\nc", PathRule::ControlChar),
            ("a/b?.csv", PathRule::ForbiddenChar),
            ("a/*.csv", PathRule::ForbiddenChar),
            ("a/<b>", PathRule::ForbiddenChar),
            ("a/b.", PathRule::TrailingDotOrSpace),
            ("a/b ", PathRule::TrailingDotOrSpace),
            ("NUL", PathRule::ReservedName),
            ("nul.csv", PathRule::ReservedName),
            ("a/Con.txt", PathRule::ReservedName),
            ("a/COM1", PathRule::ReservedName),
            ("lpt9.x.y", PathRule::ReservedName),
            ("COM\u{b9}", PathRule::ReservedName),
            ("CONIN$", PathRule::ReservedName),
            ("aux ", PathRule::TrailingDotOrSpace),
        ];
        for (path, rule) in cases {
            assert_eq!(rule_of(path), rule, "{path:?}");
        }
    }

    #[test]
    fn error_message_names_path_and_rule() {
        let msg = check_relative("../x").unwrap_err().to_string();
        assert!(msg.contains("\"../x\"") && msg.contains("'..'") && msg.contains("relatif"));
    }

    struct Env {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        outside: PathBuf,
    }

    /// <tmp>/root (MOD_ROOT) et <tmp>/outside (avec secret.csv).
    fn env() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(root.join("data")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(root.join("data/a.csv"), "x").unwrap();
        fs::write(outside.join("secret.csv"), "s").unwrap();
        Env {
            _tmp: tmp,
            root,
            outside,
        }
    }

    #[test]
    fn plain_read_and_write_resolution() {
        let e = env();
        let sb = Sandbox::new(&e.root).unwrap();
        let read = sb.resolve_read("data/a.csv").unwrap();
        assert!(read.starts_with(sb.root()) && read.is_file());
        let w = sb.resolve_write("data/new.csv").unwrap();
        assert_eq!(w, sb.root().join("data").join("new.csv"));
        let deep = sb.resolve_write("x/y/z.csv").unwrap(); // dossiers à créer
        assert_eq!(deep, sb.root().join("x").join("y").join("z.csv"));
        assert!(!sb.root().join("x").exists(), "rien n'est créé");
        assert!(sb.resolve_write("data/a.csv").is_ok()); // écrasement d'un fichier normal
        assert!(matches!(
            sb.resolve_read("data/missing.csv"),
            Err(SandboxError::NotFound { .. })
        ));
    }

    #[test]
    fn bad_roots() {
        let e = env();
        assert!(matches!(
            Sandbox::new(&e.root.join("nope")),
            Err(SandboxError::BadRoot { .. })
        ));
        assert!(matches!(
            Sandbox::new(&e.root.join("data/a.csv")),
            Err(SandboxError::BadRoot { .. })
        ));
    }

    #[test]
    fn write_into_a_file_as_if_it_were_a_dir() {
        let e = env();
        let sb = Sandbox::new(&e.root).unwrap();
        assert!(matches!(
            sb.resolve_write("data/a.csv/sub.csv"),
            Err(SandboxError::ParentNotDirectory { .. })
        ));
    }

    #[test]
    fn lexical_rules_apply_before_touching_the_disk() {
        let e = env();
        let sb = Sandbox::new(&e.root).unwrap();
        for f in [Sandbox::resolve_read, Sandbox::resolve_write] {
            assert!(matches!(
                f(&sb, "../outside/secret.csv"),
                Err(SandboxError::InvalidPath {
                    rule: PathRule::ParentDir,
                    ..
                })
            ));
        }
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::os::unix::fs::symlink;

        #[test]
        fn symlinked_file_to_outside() {
            let e = env();
            symlink(e.outside.join("secret.csv"), e.root.join("data/link.csv")).unwrap();
            let sb = Sandbox::new(&e.root).unwrap();
            assert!(matches!(
                sb.resolve_read("data/link.csv"),
                Err(SandboxError::Escapes { .. })
            ));
            assert!(matches!(
                sb.resolve_write("data/link.csv"),
                Err(SandboxError::WriteThroughLink { .. })
            ));
        }

        #[test]
        fn symlinked_dir_to_outside() {
            let e = env();
            symlink(&e.outside, e.root.join("out")).unwrap();
            let sb = Sandbox::new(&e.root).unwrap();
            assert!(matches!(
                sb.resolve_read("out/secret.csv"),
                Err(SandboxError::Escapes { .. })
            ));
            assert!(matches!(
                sb.resolve_write("out/new.csv"),
                Err(SandboxError::Escapes { .. })
            ));
            assert!(matches!(
                sb.resolve_write("out/deeper/new.csv"),
                Err(SandboxError::Escapes { .. })
            ));
            assert!(matches!(
                sb.resolve_write("out"),
                Err(SandboxError::WriteThroughLink { .. })
            ));
        }

        #[test]
        fn chain_of_symlinks_to_outside() {
            let e = env();
            symlink(&e.outside, e.root.join("hop2")).unwrap();
            symlink(e.root.join("hop2"), e.root.join("hop1")).unwrap();
            let sb = Sandbox::new(&e.root).unwrap();
            assert!(matches!(
                sb.resolve_read("hop1/secret.csv"),
                Err(SandboxError::Escapes { .. })
            ));
        }

        #[test]
        fn dangling_symlink_is_refused() {
            let e = env();
            symlink(e.outside.join("not-yet.csv"), e.root.join("dangling.csv")).unwrap();
            let sb = Sandbox::new(&e.root).unwrap();
            assert!(matches!(
                sb.resolve_write("dangling.csv"),
                Err(SandboxError::WriteThroughLink { .. })
            ));
            assert!(matches!(
                sb.resolve_read("dangling.csv"),
                Err(SandboxError::WriteThroughLink { .. })
            ));
        }

        #[test]
        fn symlink_staying_inside_is_allowed_for_reading() {
            let e = env();
            symlink(e.root.join("data"), e.root.join("alias")).unwrap();
            let sb = Sandbox::new(&e.root).unwrap();
            let p = sb.resolve_read("alias/a.csv").unwrap();
            assert_eq!(p, sb.root().join("data").join("a.csv"));
        }

        #[test]
        fn mod_root_itself_may_be_a_symlink() {
            let e = env();
            let link = e.root.parent().unwrap().join("root_link");
            symlink(&e.root, &link).unwrap();
            let sb = Sandbox::new(&link).unwrap();
            assert_eq!(sb.root(), e.root.canonicalize().unwrap());
            assert!(sb.resolve_read("data/a.csv").is_ok());
        }
    }

    #[cfg(windows)]
    mod windows {
        use super::*;
        use std::process::Command;

        /// Crée une jonction NTFS (sans privilège) ; pas d'API std, donc `mklink /J`.
        fn junction(link: &Path, target: &Path) {
            let out = Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "mklink /J a échoué: {}",
                String::from_utf8_lossy(&out.stdout)
            );
        }

        #[test]
        fn junction_to_outside() {
            let e = env();
            junction(&e.root.join("out"), &e.outside);
            let sb = Sandbox::new(&e.root).unwrap();
            assert!(matches!(
                sb.resolve_read("out/secret.csv"),
                Err(SandboxError::Escapes { .. })
            ));
            assert!(matches!(
                sb.resolve_write("out/new.csv"),
                Err(SandboxError::Escapes { .. })
            ));
            assert!(matches!(
                sb.resolve_write("out/deeper/new.csv"),
                Err(SandboxError::Escapes { .. })
            ));
            // la jonction elle-même comme cible d'écriture
            assert!(matches!(
                sb.resolve_write("out"),
                Err(SandboxError::WriteThroughLink { .. })
            ));
        }

        #[test]
        fn junction_staying_inside_is_allowed_for_reading() {
            let e = env();
            junction(&e.root.join("alias"), &e.root.join("data"));
            let sb = Sandbox::new(&e.root).unwrap();
            let p = sb.resolve_read("alias/a.csv").unwrap();
            assert_eq!(p, sb.root().join("data").join("a.csv"));
        }

        #[test]
        fn mod_root_itself_may_be_a_junction() {
            let e = env();
            let link = e.root.parent().unwrap().join("root_junction");
            junction(&link, &e.root);
            let sb = Sandbox::new(&link).unwrap();
            assert_eq!(sb.root(), e.root.canonicalize().unwrap());
            assert!(sb.resolve_read("data/a.csv").is_ok());
        }

        #[test]
        fn directory_symlink_to_outside() {
            let e = env();
            if let Err(err) = std::os::windows::fs::symlink_dir(&e.outside, e.root.join("out")) {
                eprintln!("SKIPPED directory_symlink_to_outside: création de symlink impossible ({err}); mode développeur ou droits admin requis");
                return;
            }
            let sb = Sandbox::new(&e.root).unwrap();
            assert!(matches!(
                sb.resolve_read("out/secret.csv"),
                Err(SandboxError::Escapes { .. })
            ));
            assert!(matches!(
                sb.resolve_write("out/new.csv"),
                Err(SandboxError::Escapes { .. })
            ));
        }

        #[test]
        fn file_symlink_to_outside() {
            let e = env();
            let link = e.root.join("data/link.csv");
            if let Err(err) =
                std::os::windows::fs::symlink_file(e.outside.join("secret.csv"), &link)
            {
                eprintln!(
                    "SKIPPED file_symlink_to_outside: création de symlink impossible ({err})"
                );
                return;
            }
            let sb = Sandbox::new(&e.root).unwrap();
            assert!(matches!(
                sb.resolve_read("data/link.csv"),
                Err(SandboxError::Escapes { .. })
            ));
            assert!(matches!(
                sb.resolve_write("data/link.csv"),
                Err(SandboxError::WriteThroughLink { .. })
            ));
        }

        #[test]
        fn mod_root_spelled_in_different_ways() {
            let e = env();
            let reference = Sandbox::new(&e.root).unwrap();
            let expected = reference.resolve_read("data/a.csv").unwrap();
            let text = e.root.to_string_lossy().into_owned();
            let variants = [
                text.to_uppercase(),
                text.to_lowercase(),
                text.replace('\\', "/"),
                format!("{}\\", text),
                format!("{text}\\data\\.."),
            ];
            for v in variants {
                let sb = Sandbox::new(Path::new(&v)).unwrap_or_else(|e| panic!("{v}: {e}"));
                assert_eq!(sb.root(), reference.root(), "{v}");
                assert_eq!(sb.resolve_read("data/a.csv").unwrap(), expected, "{v}");
                // un chemin existant écrit avec une autre casse reste dans la sandbox
                assert_eq!(
                    sb.resolve_read("DATA/A.CSV")
                        .unwrap()
                        .to_string_lossy()
                        .to_lowercase(),
                    expected.to_string_lossy().to_lowercase(),
                    "{v}"
                );
            }
        }

        #[test]
        fn case_variants_of_an_existing_path_resolve_to_disk_casing() {
            let e = env();
            let sb = Sandbox::new(&e.root).unwrap();
            assert_eq!(
                sb.resolve_read("DATA/A.csv").unwrap(),
                sb.root().join("data").join("a.csv")
            );
            // écriture sur un dossier existant avec une casse différente: pas de doublon
            assert_eq!(
                sb.resolve_write("DATA/new.csv").unwrap(),
                sb.root().join("data").join("new.csv")
            );
        }

        #[test]
        fn verbatim_prefix_root() {
            let e = env();
            let canonical = e.root.canonicalize().unwrap(); // commence par \\?\
            assert!(canonical.to_string_lossy().starts_with(r"\\?\"));
            let sb = Sandbox::new(&canonical).unwrap();
            assert!(sb.resolve_read("data/a.csv").is_ok());
        }

        #[test]
        fn unc_admin_share_root() {
            let e = env();
            let canonical = e.root.canonicalize().unwrap();
            let s = canonical.to_string_lossy().into_owned();
            let rest = s.trim_start_matches(r"\\?\");
            let Some((drive, tail)) = rest.split_once(r":\") else {
                eprintln!("SKIPPED unc_admin_share_root: chemin inattendu {s}");
                return;
            };
            let unc = format!(r"\\localhost\{drive}$\{tail}");
            let sb = match Sandbox::new(Path::new(&unc)) {
                Ok(sb) => sb,
                Err(err) => {
                    eprintln!(
                        "SKIPPED unc_admin_share_root: partage administratif inaccessible ({err})"
                    );
                    return;
                }
            };
            let p = sb.resolve_read("data/a.csv").unwrap();
            assert!(p.starts_with(sb.root()));
            assert!(matches!(
                sb.resolve_read("nope.csv"),
                Err(SandboxError::NotFound { .. })
            ));
        }

        #[test]
        fn root_with_spaces() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("Mon Mod (test)");
            fs::create_dir_all(root.join("sous dossier")).unwrap();
            fs::write(root.join("sous dossier/DT x.csv"), "x").unwrap();
            let sb = Sandbox::new(&root).unwrap();
            assert!(sb.resolve_read("sous dossier/DT x.csv").is_ok());
        }
    }
}
