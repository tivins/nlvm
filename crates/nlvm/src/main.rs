use std::path::Path;

use anyhow::{bail, Context, Result};

/// The `nlvm-specs` release this implementation currently targets — bump
/// `SPECS_VERSION` (repo root) whenever new specs are implemented.
const SPECS_VERSION: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../SPECS_VERSION"));

/// Recursively collects `.nlm` and `.nlp` files under `dir`, sorted for a
/// deterministic load order regardless of the OS's directory-listing order.
fn collect_nlm_modules(dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading directory {}", dir.display()))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("reading directory {}", dir.display()))?;
    entries.sort();

    for path in entries {
        if path.is_dir() {
            collect_nlm_modules(&path, out)?;
        } else if path
            .extension()
            .is_some_and(|ext| ext == "nlm" || ext == "nlp")
        {
            out.push(path.display().to_string());
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // A program can span several linked classes/interfaces, compiled by
    // `nlc` either to one `.nlm` file each or to a single `.nlp` container —
    // every `.nlm`/`.nlp` argument is loaded into one program (see
    // nl_vm::run_program). Anything else (or anything after `--`) is a
    // program argument.
    let mut module_paths = Vec::new();
    let mut program_args = Vec::new();
    let mut past_sep = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" => {
                println!(
                    "nlvm {} (nlvm-specs {})",
                    env!("CARGO_PKG_VERSION"),
                    SPECS_VERSION.trim()
                );
                return Ok(());
            }
            "-h" | "--help" => {
                println!(
                    "usage: nlvm [options] <program.nlp | module.nlm...> [--] [program args...]"
                );
                return Ok(());
            }
            "-v" | "--verbose" => {}
            "--" if !past_sep => {
                past_sep = true;
            }
            other if !past_sep && (other.ends_with(".nlm") || other.ends_with(".nlp")) => {
                module_paths.push(other.to_string())
            }
            other if !past_sep && Path::new(other).is_dir() => {
                collect_nlm_modules(Path::new(other), &mut module_paths)?
            }
            other => program_args.push(other.to_string()),
        }
        i += 1;
    }

    if module_paths.is_empty() {
        bail!("usage: nlvm [options] <program.nlp | module.nlm...> [--] [program args...]");
    }

    let mut modules = Vec::with_capacity(module_paths.len());
    for path in &module_paths {
        let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
        let loaded =
            nl_vm::load_modules(&bytes).map_err(|e| anyhow::anyhow!("loading {path}: {e}"))?;
        modules.extend(loaded);
    }

    let outcome = nl_vm::run_program(&modules, &program_args);
    print!("{}", outcome.stdout);
    if !outcome.stderr.is_empty() {
        eprintln!("{}", outcome.stderr);
    }
    std::process::exit(outcome.exit_code);
}
