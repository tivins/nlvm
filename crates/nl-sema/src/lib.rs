mod checker;
pub mod error;
mod types;

use std::collections::HashSet;

use nl_syntax::ast::{SourceFile, Type, Visibility};

pub use error::SemaError;

/// General semantic checks that apply to every program (compile-only or
/// run): definite assignment (E001), null safety (E003/E004), `auto`
/// deduction (E005), string concatenation (E008), operator compatibility
/// (E009), duplicate methods/classes (E041/E042). See compiler.md. Scoped to
/// what nl-codegen currently compiles (static methods, single class per
/// file, no cross-class calls) — the remaining checks from the 49-code list
/// land alongside the language features (objects, exceptions, templates,
/// ...) they depend on, in later phases.
pub fn check_compile(files: &[SourceFile]) -> Result<(), SemaError> {
    check_duplicate_classes(files)?;
    for file in files {
        checker::check_source_file(file)?;
    }
    Ok(())
}

fn check_duplicate_classes(files: &[SourceFile]) -> Result<(), SemaError> {
    let mut seen = HashSet::new();
    for file in files {
        let fqcn = if file.namespace.is_empty() {
            file.class.name.clone()
        } else {
            format!("{}.{}", file.namespace.join("."), file.class.name)
        };
        if !seen.insert(fqcn.clone()) {
            return Err(SemaError::DuplicateClass(fqcn));
        }
    }
    Ok(())
}

/// Entry point validation — compiler.md § Entry point validation (E027–E029).
/// Only required for "run" programs, not library/compile-only projects.
pub fn check_entry_point(files: &[SourceFile]) -> Result<(), SemaError> {
    let mut candidates = Vec::new();
    for file in files {
        for method in &file.class.methods {
            if method.name == "main" {
                candidates.push(method);
            }
        }
    }

    match candidates.len() {
        0 => Err(SemaError::NoMainMethod),
        1 => {
            let m = candidates[0];
            let is_valid_signature = m.is_static
                && m.visibility == Visibility::Public
                && m.return_type == Type::Int
                && m.params.len() == 1
                && m.params[0].ty == Type::Array(Box::new(Type::StringT));
            if is_valid_signature {
                Ok(())
            } else {
                Err(SemaError::BadMainSignature)
            }
        }
        _ => Err(SemaError::MultipleMainMethods),
    }
}
