use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// The `nlvm-specs` release this implementation currently targets — bump
/// `SPECS_VERSION` (repo root) whenever new specs are implemented.
const SPECS_VERSION: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../SPECS_VERSION"));

/// Recursively collects `.nl` files under `dir`, sorted for a deterministic
/// compilation order regardless of the OS's directory-listing order.
fn collect_nl_sources(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading directory {}", dir.display()))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("reading directory {}", dir.display()))?;
    entries.sort();

    for path in entries {
        if path.is_dir() {
            collect_nl_sources(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "nl") {
            out.push(path);
        }
    }
    Ok(())
}

/// `path:line:col: message` — mirrors `LocatedError`'s `file:line: code —
/// message` format for semantic errors, but built by hand since
/// `SyntaxError` has no file (it's parsed before `nlc` knows which source it
/// came from) and its own `Display` already spells out "lex/parse error at
/// line X, col Y" rather than the compact `path:line:col:` linter idiom.
fn format_syntax_error(path: &Path, e: &nl_syntax::SyntaxError) -> String {
    let msg = match e {
        nl_syntax::SyntaxError::Lex(m, _, _) => m,
        nl_syntax::SyntaxError::Parse(m, _, _) => m,
    };
    format!("{}:{}:{}: {msg}", path.display(), e.line(), e.col())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut output = PathBuf::from(".");
    let mut sources = Vec::new();
    let mut lint = false;
    let mut emit_modules = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                let path = args.get(i).context("missing argument for -o/--output")?;
                output = PathBuf::from(path);
            }
            "--entry" => {
                i += 1; // accepted, not yet used to pick the entry module
            }
            "-v" | "--version" => {
                println!(
                    "nlc {} (nlvm-specs {})",
                    env!("CARGO_PKG_VERSION"),
                    SPECS_VERSION.trim()
                );
                return Ok(());
            }
            "-l" | "--lint" => {
                lint = true;
            }
            "--emit-modules" => {
                emit_modules = true;
            }
            other => {
                let path = PathBuf::from(other);
                if path.is_dir() {
                    collect_nl_sources(&path, &mut sources)?;
                } else {
                    sources.push(path);
                }
            }
        }
        i += 1;
    }

    if sources.is_empty() {
        bail!("usage: nlc [options] <sources...>");
    }

    // Every source is compiled together as one program so classes/interfaces
    // defined in different files can reference each other (`new`, field
    // access, instance method calls) — see nl_codegen::compile_program.
    let mut files = Vec::with_capacity(sources.len());
    for source_path in &sources {
        let src = std::fs::read_to_string(source_path)
            .with_context(|| format!("reading {}", source_path.display()))?;
        let file = match nl_syntax::parse_source_file(&src, source_path.display().to_string()) {
            Ok(f) => f,
            Err(e) if lint => {
                eprintln!("{}", format_syntax_error(source_path, &e));
                std::process::exit(1);
            }
            Err(e) => return Err(anyhow::anyhow!("{}: {e}", source_path.display())),
        };
        files.push(file);
    }

    match nl_sema::check_compile_with_warnings(&files) {
        Ok(warnings) => {
            // compiler.md § Warnings: reported, never fail the build.
            for w in &warnings {
                eprintln!("{w}");
            }
        }
        Err(e) => {
            if lint {
                eprintln!("{e}");
                std::process::exit(1);
            }
            return Err(anyhow::anyhow!("{e}"));
        }
    }

    // `-l`/`--lint`: parse + semantic checks only, no codegen and no output
    // files — see compiler.md § Compiler invocation for the option table
    // this extends.
    if lint {
        return Ok(());
    }

    // Also includes the built-in exception hierarchy's modules (see
    // nl_syntax::prelude) — bundled alongside the caller's own classes so
    // `nlvm` can load a program that references e.g. `Exception` without
    // the caller having to know about the prelude.
    let modules = nl_codegen::compile_program(&files).map_err(|e| anyhow::anyhow!("{e}"))?;

    // `--emit-modules`: one `.nlm` file per class/interface in the output
    // directory (the historical layout). Default: a single `.nlp` program
    // container — `-o` may name the file directly (`-o prog.nlp`) or a
    // directory, in which case the file is named after the entry class.
    if emit_modules {
        std::fs::create_dir_all(&output)
            .with_context(|| format!("creating output directory {}", output.display()))?;
        for module in &modules {
            let name = module.this_class_name().unwrap_or("Unknown");
            let out_path = output.join(format!("{name}.nlm"));
            std::fs::write(&out_path, module.encode())
                .with_context(|| format!("writing {}", out_path.display()))?;
        }
        return Ok(());
    }

    let out_path = if output.extension().is_some_and(|ext| ext == "nlp") {
        if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating output directory {}", parent.display()))?;
        }
        output
    } else {
        // The entry class (static `main`) names the program. No positional
        // fallback: compile_program prepends the prelude, so the first
        // module is a built-in class, not the user's.
        let name = modules
            .iter()
            .find(|m| m.find_method("main").is_some_and(|m| m.is_static()))
            .and_then(|m| m.this_class_name())
            .unwrap_or("Program");
        std::fs::create_dir_all(&output)
            .with_context(|| format!("creating output directory {}", output.display()))?;
        output.join(format!("{name}.nlp"))
    };

    std::fs::write(&out_path, nl_bytecode::encode_program(&modules))
        .with_context(|| format!("writing {}", out_path.display()))?;

    Ok(())
}
