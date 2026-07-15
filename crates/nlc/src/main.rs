use std::path::PathBuf;

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut output_dir = PathBuf::from(".");
    let mut sources = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                let dir = args.get(i).context("missing argument for -o/--output")?;
                output_dir = PathBuf::from(dir);
            }
            "--entry" => {
                i += 1; // accepted, not yet used to pick the entry module
            }
            other => sources.push(PathBuf::from(other)),
        }
        i += 1;
    }

    if sources.is_empty() {
        bail!("usage: nlc [options] <sources...>");
    }

    std::fs::create_dir_all(&output_dir)
        .with_context(|| format!("creating output directory {}", output_dir.display()))?;

    // Every source is compiled together as one program so classes/interfaces
    // defined in different files can reference each other (`new`, field
    // access, instance method calls) — see nl_codegen::compile_program.
    let mut files = Vec::with_capacity(sources.len());
    for source_path in &sources {
        let src = std::fs::read_to_string(source_path)
            .with_context(|| format!("reading {}", source_path.display()))?;
        let file = nl_syntax::parse_source_file(&src)
            .map_err(|e| anyhow::anyhow!("{}: {e}", source_path.display()))?;
        files.push(file);
    }

    nl_sema::check_compile(&files).map_err(|e| anyhow::anyhow!("{e} ({})", e.code()))?;

    // Also includes the built-in exception hierarchy's modules (see
    // nl_syntax::prelude) — written out alongside the caller's own classes
    // so `nlvm` can load a program that references e.g. `Exception` without
    // the caller having to know about the prelude.
    let modules = nl_codegen::compile_program(&files).map_err(|e| anyhow::anyhow!("{e}"))?;

    for module in &modules {
        let name = module.this_class_name().unwrap_or("Unknown");
        let out_path = output_dir.join(format!("{name}.nlm"));
        std::fs::write(&out_path, module.encode())
            .with_context(|| format!("writing {}", out_path.display()))?;
    }

    Ok(())
}
