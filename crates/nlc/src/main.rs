use std::path::{Path, PathBuf};

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

    for source_path in &sources {
        compile_one(source_path, &output_dir)?;
    }

    Ok(())
}

fn compile_one(source_path: &Path, output_dir: &Path) -> Result<()> {
    let src = std::fs::read_to_string(source_path)
        .with_context(|| format!("reading {}", source_path.display()))?;

    let file = nl_syntax::parse_source_file(&src)
        .map_err(|e| anyhow::anyhow!("{}: {e}", source_path.display()))?;

    nl_sema::check_compile(std::slice::from_ref(&file))
        .map_err(|e| anyhow::anyhow!("{}: {e} ({})", source_path.display(), e.code()))?;

    let module = nl_codegen::compile_source_file(&file)
        .map_err(|e| anyhow::anyhow!("{}: {e}", source_path.display()))?;

    let out_path = output_dir.join(format!("{}.nlm", file.class.name));
    std::fs::write(&out_path, module.encode())
        .with_context(|| format!("writing {}", out_path.display()))?;

    Ok(())
}
