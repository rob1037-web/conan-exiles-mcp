#![cfg(windows)]
//! Test sur le vrai disque via MCP: MOD_ROOT réel (lecture seule, jamais écrit) et CSV_ROOT par
//! défaut `D:\ConanMod\csv`. Ignoré (ligne `SKIPPED`) si le dossier du mod est absent.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use common::*;
use conan_core::Config;
use serde_json::json;

const REAL_MOD_ROOT: &str = r"D:\Program Files\CEUE5Devkit\UE4\Content\Mods\test";

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

fn mod_top_level() -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(REAL_MOD_ROOT)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Supprime les fichiers de test même si une assertion échoue.
struct Cleanup(Vec<PathBuf>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = fs::remove_file(p);
        }
        // seulement si vide: ne touche jamais aux vraies sauvegardes
        let _ = fs::remove_dir(Path::new(r"D:\ConanMod\csv\.backup"));
    }
}

#[tokio::test]
async fn real_disk_end_to_end_through_mcp() {
    if !Path::new(REAL_MOD_ROOT).join("modinfo.json").is_file() {
        eprintln!(
            "SKIPPED real_disk_end_to_end_through_mcp: dossier du mod absent ({REAL_MOD_ROOT})"
        );
        return;
    }
    let mod_before = snapshot();
    let top_before = mod_top_level();

    // SCHEMA_ROOT temporaire contenant le schéma réel de la table (le vrai dossier de schémas
    // de l'utilisateur n'est pas touché).
    let schemas = tempfile::tempdir().unwrap();
    fs::write(schemas.path().join(format!("{TABLE}.toml")), SCHEMA_TOML).unwrap();
    let cfg = Config::new(REAL_MOD_ROOT, None).with_schema_root(schemas.path());
    let s = connect(&cfg).await; // crée D:\ConanMod\csv au besoin
    let name = format!(
        "_conan_mcp_test_{}_{}.csv",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let w = |content: &str, dry: Option<bool>, confirm: Option<bool>| {
        let mut v = write_args(TABLE, content, dry, confirm);
        v["path"] = json!(name);
        v
    };
    let mut cleanup = Cleanup(vec![Path::new(r"D:\ConanMod\csv").join(&name)]);
    let v2 = csv_v2();

    // le mod est listable et lisible (lecture seule)
    let r = s.call("list_mod_files", json!({"source": "mod"})).await;
    assert!(!is_error(&r), "{}", text(&r));
    let listing = serde_json::to_string(structured(&r)).unwrap();
    assert!(listing.contains("modinfo.json") && !listing.contains(".backup"));

    // dry-run par défaut: rien n'est écrit
    let r = s.call("write_datatable", w(CSV_V1, None, None)).await;
    assert!(text(&r).starts_with("DRY-RUN"), "{}", text(&r));
    assert!(!cleanup.0[0].exists());

    // dry-run invalide: rapport d'erreurs, toujours rien d'écrit; écriture réelle refusée
    let invalid = CSV_V1.replace("\"5.000000\"", "\"cinq\"");
    let r = s.call("write_datatable", w(&invalid, None, None)).await;
    assert!(
        text(&r).starts_with("DRY-RUN — VALIDATION ÉCHOUÉE"),
        "{}",
        text(&r)
    );
    let r = s
        .call("write_datatable", w(&invalid, Some(false), Some(true)))
        .await;
    assert!(
        is_error(&r) && text(&r).starts_with("ERREUR ValidationFailed:"),
        "{}",
        text(&r)
    );
    assert!(!cleanup.0[0].exists());

    // création réelle, relecture
    let r = s
        .call("write_datatable", w(CSV_V1, Some(false), Some(true)))
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(fs::read_to_string(&cleanup.0[0]).unwrap(), CSV_V1);
    let r = s.call("read_datatable", json!({"path": name})).await;
    assert_eq!(structured(&r)["rows"][0][0], "Trash1");

    // modification confirmée: sauvegarde identique à l'original, absente du listage
    let r = s
        .call("write_datatable", w(&v2, Some(false), Some(true)))
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    let backup = PathBuf::from(structured(&r)["backup_real_path"].as_str().unwrap());
    cleanup.0.push(backup.clone());
    assert!(backup.starts_with(r"D:\ConanMod\csv\.backup"), "{backup:?}");
    assert_eq!(fs::read_to_string(&backup).unwrap(), CSV_V1);
    assert_eq!(fs::read_to_string(&cleanup.0[0]).unwrap(), v2);
    let r = s.call("list_mod_files", json!({})).await;
    let csv_listing = serde_json::to_string(structured(&r)).unwrap();
    assert!(
        csv_listing.contains(&name) && !csv_listing.contains(".backup"),
        "{csv_listing}"
    );

    // tentatives vers le mod: refusées
    for path in ["../Program Files/x.csv", "D:/Program Files/x.csv"] {
        let mut args = write_args(TABLE, CSV_V1, Some(false), Some(true));
        args["path"] = json!(path);
        let r = s.call("write_datatable", args).await;
        assert!(is_error(&r), "{path}");
    }

    // nettoyage explicite puis vérification du mod
    drop(cleanup);
    assert_eq!(
        snapshot(),
        mod_before,
        "modinfo.json ou active.txt a changé"
    );
    assert_eq!(
        mod_top_level(),
        top_before,
        "le contenu du dossier du mod a changé"
    );
}
