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
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM1", "COM2", "COM3", "COM4", "COM5",
    "COM6", "COM7", "COM8", "COM9", "COM\u{b9}", "COM\u{b2}", "COM\u{b3}", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT\u{b9}", "LPT\u{b2}", "LPT\u{b3}",
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
            let candidate = comps[..depth].iter().fold(self.root.clone(), |p, c| p.join(c));
            let meta = match std::fs::symlink_metadata(&candidate) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
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
