use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut module_path = None;
    let mut program_args = Vec::new();
    let mut i = 0;
    let mut past_module = false;
    while i < args.len() {
        match args[i].as_str() {
            "--version" => {
                println!("nlvm 0.1.0");
                return Ok(());
            }
            "-h" | "--help" => {
                println!("usage: nlvm [options] <module-or-program> [--] [program args...]");
                return Ok(());
            }
            "-v" | "--verbose" => {}
            "--" if !past_module => {
                // Everything after this point is program args, even before a module is set.
            }
            other if module_path.is_none() && !past_module => {
                module_path = Some(other.to_string());
                past_module = true;
            }
            other => program_args.push(other.to_string()),
        }
        i += 1;
    }

    let Some(module_path) = module_path else {
        bail!("usage: nlvm [options] <module-or-program> [--] [program args...]");
    };

    let bytes = std::fs::read(&module_path).with_context(|| format!("reading {module_path}"))?;
    let module = nl_vm::load_module(&bytes).with_context(|| format!("loading {module_path}"))?;

    let outcome = nl_vm::run_program(&module, &program_args);
    print!("{}", outcome.stdout);
    if !outcome.stderr.is_empty() {
        eprintln!("{}", outcome.stderr);
    }
    std::process::exit(outcome.exit_code);
}
