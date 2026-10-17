//! Montre ce que décide la sandbox pour un MOD_ROOT et un chemin relatif réels.
//!
//!     sandbox_probe <MOD_ROOT> <chemin/relatif> [--write]
//!
//! Sans `--write` : résolution en lecture (le chemin doit exister).
//! Avec `--write` : résolution en écriture (rien n'est créé).
//! Code de sortie 0 si accepté, 1 si refusé, 2 si mauvais usage.

use std::path::Path;
use std::process::ExitCode;

use conan_core::Sandbox;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let write = args.iter().any(|a| a == "--write");
    let pos: Vec<&String> = args.iter().filter(|a| *a != "--write").collect();
    let [root, rel] = pos.as_slice() else {
        eprintln!("usage: sandbox_probe <MOD_ROOT> <chemin/relatif> [--write]");
        return ExitCode::from(2);
    };
    let sandbox = match Sandbox::new(Path::new(root.as_str())) {
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
