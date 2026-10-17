//! Montre ce que décide la sandbox pour une racine et un chemin relatif réels.
//!
//!     sandbox_probe <ROOT> <chemin/relatif> [--write]
//!     sandbox_probe --csv <chemin/relatif>          # ROOT = CSV_ROOT (env, défaut D:\ConanMod\csv)
//!
//! Sans `--write` : résolution en lecture (le chemin doit exister).
//! Avec `--write` : résolution en écriture. La sonde ne crée jamais rien, pas même
//! CSV_ROOT : s'il n'existe pas encore, elle le signale et ne contrôle que le chemin relatif.
//! Code de sortie 0 si accepté, 1 si refusé, 2 si mauvais usage.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use conan_core::{check_relative, Sandbox, DEFAULT_CSV_ROOT};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let csv_mode = args.first().is_some_and(|a| a == "--csv");
    if csv_mode {
        args.remove(0);
    }
    let write = args.iter().any(|a| a == "--write");
    let pos: Vec<&String> = args.iter().filter(|a| *a != "--write").collect();

    let (root, rel): (PathBuf, &String) = match (csv_mode, pos.as_slice()) {
        (true, [rel]) => {
            let root = std::env::var_os("CSV_ROOT")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_CSV_ROOT));
            (root, rel)
        }
        (false, [root, rel]) => (PathBuf::from(root.as_str()), rel),
        _ => {
            eprintln!("usage: sandbox_probe <ROOT> <chemin/relatif> [--write]");
            eprintln!("       sandbox_probe --csv <chemin/relatif> [--write]");
            return ExitCode::from(2);
        }
    };

    if csv_mode {
        println!("CSV_ROOT : {}", root.display());
        if !root.exists() {
            println!("CSV_ROOT n'existe pas encore (créé à la première écriture); rien n'est créé par la sonde");
            return match check_relative(rel) {
                Ok(_) => {
                    println!("ACCEPTÉ (contrôle lexical seul): {rel}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    println!("REFUSÉ: {e}");
                    ExitCode::FAILURE
                }
            };
        }
    }

    let sandbox = match Sandbox::new(Path::new(&root)) {
        Ok(s) => s,
        Err(e) => {
            println!("REFUSÉ: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("racine canonique : {}", sandbox.root().display());
    let result = if write {
        sandbox.resolve_write(rel)
    } else {
        sandbox.resolve_read(rel)
    };
    match result {
        Ok(p) => {
            println!("ACCEPTÉ: {}", p.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("REFUSÉ: {e}");
            ExitCode::FAILURE
        }
    }
}
