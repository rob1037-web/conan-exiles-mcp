//! Démarrage: configuration valide ou refus avec un message clair.

use std::ffi::OsString;
use std::fs;

use mcp_server::build_server_from_vars;

fn vars(pairs: Vec<(&'static str, OsString)>) -> impl Fn(&str) -> Option<OsString> {
    move |name| {
        pairs
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.clone())
    }
}

fn error_text(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[test]
fn starts_with_valid_config_and_creates_csv_root() {
    let tmp = tempfile::tempdir().unwrap();
    let mod_root = tmp.path().join("mod");
    fs::create_dir_all(&mod_root).unwrap();
    let csv_root = tmp.path().join("data").join("csv");
    let get = vars(vec![
        ("MOD_ROOT", mod_root.clone().into()),
        ("CSV_ROOT", csv_root.clone().into()),
    ]);
    assert!(build_server_from_vars(get).is_ok());
    assert!(csv_root.is_dir());
}

#[test]
fn refuses_without_mod_root() {
    let msg = error_text(build_server_from_vars(vars(vec![])).err().unwrap());
    assert!(msg.contains("MOD_ROOT"), "{msg}");
}

#[test]
fn refuses_empty_csv_root_and_missing_mod_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let get = vars(vec![
        ("MOD_ROOT", tmp.path().into()),
        ("CSV_ROOT", OsString::new()),
    ]);
    let msg = error_text(build_server_from_vars(get).err().unwrap());
    assert!(msg.contains("CSV_ROOT"), "{msg}");

    let absent = tmp.path().join("absent");
    let get = vars(vec![
        ("MOD_ROOT", absent.into()),
        ("CSV_ROOT", tmp.path().join("csv").into()),
    ]);
    let msg = error_text(build_server_from_vars(get).err().unwrap());
    assert!(msg.contains("MOD_ROOT"), "{msg}");
    assert!(
        !tmp.path().join("csv").exists(),
        "rien n'est créé si la config est invalide"
    );
}

#[test]
fn refuses_csv_root_inside_mod_root_without_creating_anything() {
    let tmp = tempfile::tempdir().unwrap();
    let mod_root = tmp.path().join("mod");
    fs::create_dir_all(&mod_root).unwrap();
    let get = vars(vec![
        ("MOD_ROOT", mod_root.clone().into()),
        ("CSV_ROOT", mod_root.join("csv").into()),
    ]);
    let msg = error_text(build_server_from_vars(get).err().unwrap());
    assert!(msg.contains("lecture seule"), "{msg}");
    assert!(!mod_root.join("csv").exists());
}

#[test]
fn schema_root_absent_or_invalid_never_prevents_startup() {
    let tmp = tempfile::tempdir().unwrap();
    let mod_root = tmp.path().join("mod");
    fs::create_dir_all(&mod_root).unwrap();
    let base = |schema: Option<OsString>| {
        let mut v: Vec<(&'static str, OsString)> = vec![
            ("MOD_ROOT", mod_root.clone().into()),
            ("CSV_ROOT", tmp.path().join("csv").into()),
        ];
        if let Some(s) = schema {
            v.push(("SCHEMA_ROOT", s));
        }
        vars(v)
    };
    assert!(build_server_from_vars(base(None)).is_ok());
    assert!(build_server_from_vars(base(Some(OsString::new()))).is_ok());
    assert!(build_server_from_vars(base(Some(tmp.path().join("absent").into()))).is_ok());
    let schemas = tmp.path().join("schemas");
    fs::create_dir_all(&schemas).unwrap();
    assert!(build_server_from_vars(base(Some(schemas.into()))).is_ok());
}
