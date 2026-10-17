//! Configuration des deux racines, distinctes :
//!
//! - `MOD_ROOT` : dossier du mod (Devkit). **Lecture seule** pour ce projet.
//! - `CSV_ROOT` : dossier où le serveur écrit les CSV, hors du dossier du mod.
//!   Défaut : `D:\ConanMod\csv`. L'import CSV → DataTable reste manuel dans le Devkit.
//!
//! Les deux se règlent par variables d'environnement du même nom. `MOD_ROOT` n'a pas de
//! défaut : il doit être fourni. Aucun accès disque ici, la config est un simple conteneur.

use std::ffi::OsString;
use std::path::PathBuf;

/// Valeur par défaut de `CSV_ROOT`.
pub const DEFAULT_CSV_ROOT: &str = r"D:\ConanMod\csv";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("variable d'environnement MOD_ROOT absente ou vide: indiquer le dossier du mod (lecture seule), par exemple D:\\Program Files\\...\\Mods\\<mod>")]
    MissingModRoot,
    #[error("variable d'environnement CSV_ROOT vide: la retirer pour utiliser le défaut ({DEFAULT_CSV_ROOT}) ou donner un dossier")]
    EmptyCsvRoot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Dossier du mod, en lecture seule.
    pub mod_root: PathBuf,
    /// Dossier d'écriture des CSV.
    pub csv_root: PathBuf,
}

impl Config {
    /// `csv_root` à `None` : défaut `D:\ConanMod\csv`.
    pub fn new(mod_root: impl Into<PathBuf>, csv_root: Option<PathBuf>) -> Config {
        Config {
            mod_root: mod_root.into(),
            csv_root: csv_root.unwrap_or_else(|| PathBuf::from(DEFAULT_CSV_ROOT)),
        }
    }

    /// Lit `MOD_ROOT` et `CSV_ROOT` dans l'environnement du processus.
    pub fn from_env() -> Result<Config, ConfigError> {
        Config::from_vars(|name| std::env::var_os(name))
    }

    /// Comme `from_env`, avec une source de variables injectable (tests).
    pub fn from_vars(get: impl Fn(&str) -> Option<OsString>) -> Result<Config, ConfigError> {
        let mod_root = get("MOD_ROOT")
            .filter(|v| !v.is_empty())
            .ok_or(ConfigError::MissingModRoot)?;
        let csv_root = match get("CSV_ROOT") {
            None => None,
            Some(v) if v.is_empty() => return Err(ConfigError::EmptyCsvRoot),
            Some(v) => Some(PathBuf::from(v)),
        };
        Ok(Config::new(mod_root, csv_root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn csv_root_defaults_to_the_conan_mod_folder() {
        let c = Config::from_vars(vars(&[("MOD_ROOT", r"D:\Program Files\X\Mods\test")])).unwrap();
        assert_eq!(c.mod_root, PathBuf::from(r"D:\Program Files\X\Mods\test"));
        assert_eq!(c.csv_root, PathBuf::from(r"D:\ConanMod\csv"));
    }

    #[test]
    fn csv_root_is_overridable_and_independent_of_mod_root() {
        let c = Config::from_vars(vars(&[("MOD_ROOT", "/m"), ("CSV_ROOT", "/c")])).unwrap();
        assert_eq!(c, Config::new("/m", Some(PathBuf::from("/c"))));
        assert_ne!(c.mod_root, c.csv_root);
    }

    #[test]
    fn mod_root_is_required() {
        assert_eq!(
            Config::from_vars(vars(&[])).unwrap_err(),
            ConfigError::MissingModRoot
        );
        assert_eq!(
            Config::from_vars(vars(&[("MOD_ROOT", ""), ("CSV_ROOT", "/c")])).unwrap_err(),
            ConfigError::MissingModRoot
        );
    }

    #[test]
    fn empty_csv_root_is_an_error_not_a_silent_default() {
        assert_eq!(
            Config::from_vars(vars(&[("MOD_ROOT", "/m"), ("CSV_ROOT", "")])).unwrap_err(),
            ConfigError::EmptyCsvRoot
        );
    }
}
