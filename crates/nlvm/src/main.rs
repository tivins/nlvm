use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // A program can span several linked classes/interfaces, each compiled
    // to its own `.nlm` file by `nlc` — every `.nlm` argument is loaded into
    // one program (see nl_vm::run_program). Anything else (or anything after
    // `--`) is a program argument.
    let mut module_paths = Vec::new();
    let mut program_args = Vec::new();
    let mut past_sep = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" => {
                println!("nlvm 0.1.0");
                return Ok(());
            }
            "-h" | "--help" => {
                println!("usage: nlvm [options] <module.nlm...> [--] [program args...]");
                return Ok(());
            }
            "-v" | "--verbose" => {}
            "--" if !past_sep => {
                past_sep = true;
            }
            other if !past_sep && other.ends_with(".nlm") => module_paths.push(other.to_string()),
            other => program_args.push(other.to_string()),
        }
        i += 1;
    }

    if module_paths.is_empty() {
        bail!("usage: nlvm [options] <module.nlm...> [--] [program args...]");
    }

    let mut modules = Vec::with_capacity(module_paths.len());
    for path in &module_paths {
        let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
        let module = nl_vm::load_module(&bytes).map_err(|e| anyhow::anyhow!("loading {path}: {e}"))?;
        modules.push(module);
    }

    let outcome = nl_vm::run_program(&modules, &program_args);
    print!("{}", outcome.stdout);
    if !outcome.stderr.is_empty() {
        eprintln!("{}", outcome.stderr);
    }
    std::process::exit(outcome.exit_code);
}
