mod header;
mod runner;
mod testfile;

use anyhow::{Context, Result};

fn main() -> Result<()> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/data/projects/nlvm-specs/tests".to_string());

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading test directory {dir}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "yaml"))
        .collect();
    entries.sort();

    let mut passed = 0;
    let mut failed = 0;

    for path in &entries {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;

        let test = match testfile::parse_test_file(&content) {
            Ok(t) => t,
            Err(e) => {
                println!("FAIL {name}: malformed test file: {e}");
                failed += 1;
                continue;
            }
        };

        match runner::run_test(&test) {
            runner::Outcome::Pass => {
                println!("PASS {name}");
                passed += 1;
            }
            runner::Outcome::Fail(reason) => {
                println!("FAIL {name}: {reason}");
                failed += 1;
            }
        }
    }

    println!("---");
    println!("{passed} passed, {failed} failed, {} total", passed + failed);

    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
