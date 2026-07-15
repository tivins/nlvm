mod checker;
mod class_table;
pub mod error;
mod types;

use std::collections::HashSet;

use nl_syntax::ast::{SourceFile, SourceItem, Type, Visibility};

pub use error::SemaError;

/// General semantic checks that apply to every program (compile-only or
/// run): definite assignment (E001), null safety (E003/E004), `auto`
/// deduction (E005), string concatenation (E008), operator compatibility
/// (E009), duplicate methods/classes (E041/E042), constructor delegation
/// (E045/E046). See compiler.md. Cross-file class/field/method references
/// (objects, `new`, arrays, interfaces — milestone 5) are checked leniently:
/// an unresolved class/field/method defers to nl-codegen's harder error,
/// same as unresolved calls already did before this phase.
pub fn check_compile(files: &[SourceFile]) -> Result<(), SemaError> {
    // Built-in exception classes (nl_syntax::prelude) are implicitly part of
    // every program — see class_table::import_map, which seeds their simple
    // names so user code can reference them without a `use`.
    let mut all_files = nl_syntax::prelude::files();
    all_files.extend_from_slice(files);

    check_duplicate_classes(&all_files)?;
    let classes = class_table::build_class_table(&all_files);
    for file in &all_files {
        checker::check_source_file(file, &classes)?;
    }
    Ok(())
}

fn check_duplicate_classes(files: &[SourceFile]) -> Result<(), SemaError> {
    let mut seen = HashSet::new();
    for file in files {
        let fqcn = class_table::fqcn_of(file);
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
        let SourceItem::Class(class) = &file.item else {
            continue;
        };
        for method in &class.methods {
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
