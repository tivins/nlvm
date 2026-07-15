pub mod error;

use nl_syntax::ast::{SourceFile, Type, Visibility};

pub use error::SemaError;

/// General semantic checks that apply to every program (compile-only or run).
/// Currently a no-op placeholder — real checks (definite assignment, null
/// safety, type checking, etc. — compiler.md's 49 error codes) land here
/// incrementally as later milestones are implemented.
pub fn check_compile(_files: &[SourceFile]) -> Result<(), SemaError> {
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
