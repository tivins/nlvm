use nl_bytecode::Module;

use crate::header::Header;
use crate::testfile::TestFile;

pub enum Outcome {
    Pass,
    Fail(String),
}

pub fn run_test(test: &TestFile) -> Outcome {
    let mut files = Vec::new();
    for block in &test.blocks {
        match nl_syntax::parse_source_file(&block.content, block.path.clone()) {
            Ok(f) => files.push(f),
            Err(e) => {
                return if test.header.expected_parse_error == Some(true) {
                    Outcome::Pass
                } else {
                    Outcome::Fail(format!("parse error in {}: {e}", block.path))
                };
            }
        }
    }
    if test.header.expected_parse_error == Some(true) {
        return Outcome::Fail("expected a parse error but parsing succeeded".to_string());
    }

    let warnings = match nl_sema::check_compile_with_warnings(&files) {
        Ok(warnings) => warnings,
        Err(e) => {
            return match &test.header.expected_compile_error {
                Some(code) if code == e.code() => Outcome::Pass,
                Some(code) => Outcome::Fail(format!(
                    "expected compile error {code}, got {} ({e})",
                    e.code()
                )),
                None => Outcome::Fail(format!("unexpected compile error: {e}")),
            };
        }
    };
    if let Some(code) = &test.header.expected_compile_error {
        return Outcome::Fail(format!(
            "expected compile error {code} but compilation succeeded"
        ));
    }
    if let Some(code) = &test.header.expected_warning {
        if !warnings.iter().any(|w| w.code() == code) {
            return Outcome::Fail(format!(
                "expected warning {code} but it was not reported (got: [{}])",
                warnings
                    .iter()
                    .map(|w| w.code())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    let modules = match nl_codegen::compile_program(&files) {
        Ok(m) => m,
        Err(e) => return Outcome::Fail(format!("codegen error: {e}")),
    };

    if let Some(msg) = check_module_structure(&test.header, &modules) {
        return Outcome::Fail(msg);
    }

    if test.header.is_compile_only() {
        return Outcome::Pass;
    }

    if test.header.expected_exit_code.is_none() {
        return Outcome::Pass;
    }

    if let Err(e) = nl_sema::check_entry_point(&files) {
        return Outcome::Fail(format!("entry point check failed: {e}"));
    }

    if !modules.iter().any(|m| m.find_method("main").is_some()) {
        return Outcome::Fail("no module with 'main' found after codegen".to_string());
    }

    let run_outcome = match &test.header.stdin {
        Some(input) => nl_vm::run_program_with_stdin(&modules, &[], input),
        None => nl_vm::run_program(&modules, &[]),
    };

    if let Some(expected) = test.header.expected_exit_code {
        if run_outcome.exit_code != expected {
            let detail = if run_outcome.stderr.is_empty() {
                String::new()
            } else {
                format!(" ({})", run_outcome.stderr)
            };
            return Outcome::Fail(format!(
                "exit code mismatch: expected {expected}, got {}{detail}",
                run_outcome.exit_code
            ));
        }
    }
    if let Some(expected) = &test.header.expected_stdout {
        if &run_outcome.stdout != expected {
            return Outcome::Fail(format!(
                "stdout mismatch: expected {expected:?}, got {:?}",
                run_outcome.stdout
            ));
        }
    }
    if let Some(expected) = &test.header.expected_stderr {
        if &run_outcome.stderr != expected {
            return Outcome::Fail(format!(
                "stderr mismatch: expected {expected:?}, got {:?}",
                run_outcome.stderr
            ));
        }
    }

    Outcome::Pass
}

fn check_module_structure(header: &Header, modules: &[Module]) -> Option<String> {
    if let Some(expected_class) = &header.expected_class {
        let found = modules
            .iter()
            .any(|m| m.this_class_name() == Some(expected_class.as_str()));
        if !found {
            return Some(format!(
                "expected_class '{expected_class}' not found in any compiled module"
            ));
        }
    }

    if let Some(expected_methods) = &header.expected_methods {
        for name in expected_methods {
            let found = modules.iter().any(|m| {
                m.methods
                    .iter()
                    .any(|meth| m.constant_pool.utf8_at(meth.name_index) == Some(name.as_str()))
            });
            if !found {
                return Some(format!(
                    "expected method '{name}' not found in any compiled module"
                ));
            }
        }
    }

    if let Some(expected_fields) = &header.expected_fields {
        for entry in expected_fields {
            let (name, ty) = match entry {
                serde_yaml::Value::String(s) => (s.clone(), None),
                serde_yaml::Value::Mapping(map) => {
                    let name = map
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let ty = map
                        .get("type")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    (name, ty)
                }
                _ => continue,
            };
            let found = modules.iter().any(|m| {
                m.fields.iter().any(|f| {
                    let name_matches = m.constant_pool.utf8_at(f.name_index) == Some(name.as_str());
                    let type_matches = ty
                        .as_deref()
                        .is_none_or(|t| m.constant_pool.type_desc_at(f.type_index) == Some(t));
                    name_matches && type_matches
                })
            });
            if !found {
                return Some(format!(
                    "expected field '{name}' not found in any compiled module"
                ));
            }
        }
    }

    if let Some(expected_cp) = &header.expected_constant_pool_contains {
        for needle in expected_cp {
            let found = modules.iter().any(|m| {
                m.constant_pool.entries().iter().any(|e| match e {
                    nl_bytecode::ConstantPoolEntry::Utf8(s) => s == needle,
                    _ => false,
                })
            });
            if !found {
                return Some(format!(
                    "constant pool entry '{needle}' not found in any compiled module"
                ));
            }
        }
    }

    None
}
