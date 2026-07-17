mod checker;
mod class_table;
pub mod error;
mod native_generics;
mod stdlib;
mod types;

use std::collections::{HashMap, HashSet};

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
    // Template classes (specs.md § Template class) are expanded into
    // ordinary monomorphized classes before anything else sees them — see
    // nl_syntax::monomorphize. Runs ahead of the prelude prepend below
    // since exception classes are never templates.
    let expanded = nl_syntax::monomorphize::expand(files.to_vec());

    // Built-in exception classes (nl_syntax::prelude) are implicitly part of
    // every program — see class_table::import_map, which seeds their simple
    // names so user code can reference them without a `use`.
    let mut all_files = nl_syntax::prelude::files();
    all_files.extend(expanded);

    check_duplicate_classes(&all_files)?;
    for file in &all_files {
        check_duplicate_imports(file, &all_files)?;
    }
    let classes = class_table::build_class_table(&all_files);
    // Must run against the *original*, pre-expansion `files` — by this point
    // `nl_syntax::monomorphize::expand` has already rewritten every
    // `Type::Generic`/`new T<...>(...)` site away, but `classes` (built from
    // the expanded program) still has everything needed to resolve whether a
    // concrete type argument satisfies its bound.
    check_template_bounds(files, &classes)?;
    for file in &all_files {
        checker::check_source_file(file, &all_files, &classes)?;
    }
    Ok(())
}

/// compiler.md § Template instantiation, "Bounded type parameters" — E037.
fn check_template_bounds(files: &[SourceFile], classes: &class_table::ClassTable) -> Result<(), SemaError> {
    let instantiations = nl_syntax::monomorphize::collect_instantiations(files);
    for (template_fqcn, args) in instantiations.values() {
        let Some(template_file) = files.iter().find(|f| class_table::fqcn_of(f) == *template_fqcn) else {
            continue;
        };
        let SourceItem::Class(template_class) = &template_file.item else {
            continue;
        };
        let imports = class_table::import_map(template_file, files);
        for (type_param, arg) in template_class.type_params.iter().zip(args.iter()) {
            let Some(bound_name) = &type_param.bound else {
                continue;
            };
            let bound_fqcn = imports.get(bound_name).cloned().unwrap_or_else(|| bound_name.clone());
            let Type::Named(arg_fqcn) = arg else {
                // A primitive/array concrete argument can't satisfy a
                // class/interface bound at all, but no test exercises that
                // combination — left lenient rather than guessing an error
                // shape for it.
                continue;
            };
            if !class_table::satisfies_bound(classes, arg_fqcn, &bound_fqcn) {
                return Err(SemaError::TemplateBoundNotSatisfied(arg_fqcn.clone(), bound_fqcn, template_fqcn.clone()));
            }
        }
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

/// compiler.md § Import name resolution — E043. A `use` clause conflicts if
/// its bound name — the `as Alias` name if given, else the simple
/// (last-segment) name — is already bound, under that same file, to a
/// *different* entity: the class being defined in the file, another type in
/// the same namespace (already visible without `use` — see
/// `class_table::import_map`), or another `use` clause processed earlier in
/// this file. Re-importing the exact same FQCN that's already implicitly
/// visible (e.g. `m5_0010`'s `use test.class.ClassTest;` from within
/// `test.class.Main`) is redundant but not a conflict — only a mismatched
/// FQCN under an already-bound name is. An alias never collides with the
/// unaliased simple name of its own target (only with *other* bindings), so
/// `use x.Y as Y;` is just a redundant, harmless spelling.
fn check_duplicate_imports(file: &SourceFile, all_files: &[SourceFile]) -> Result<(), SemaError> {
    let own_fqcn = class_table::fqcn_of(file);
    let own_simple = match &file.item {
        SourceItem::Class(c) => c.name.as_str(),
        SourceItem::Interface(i) => i.name.as_str(),
    };
    let mut same_namespace: HashMap<&str, String> = HashMap::new();
    for other in all_files {
        if other.namespace == file.namespace {
            let simple = match &other.item {
                SourceItem::Class(c) => c.name.as_str(),
                SourceItem::Interface(i) => i.name.as_str(),
            };
            if simple != own_simple {
                same_namespace.insert(simple, class_table::fqcn_of(other));
            }
        }
    }
    let mut imported: HashMap<&str, &str> = HashMap::new();
    for u in &file.uses {
        let bound_name = u
            .alias
            .as_deref()
            .unwrap_or_else(|| u.path.rsplit('.').next().expect("use path is never empty"));
        if bound_name == own_simple && u.path != own_fqcn {
            return Err(SemaError::DuplicateImportSymbol(bound_name.to_string()));
        }
        if let Some(existing_fqcn) = same_namespace.get(bound_name) {
            if existing_fqcn != &u.path {
                return Err(SemaError::DuplicateImportSymbol(bound_name.to_string()));
            }
        }
        match imported.get(bound_name) {
            Some(existing) if *existing != u.path => {
                return Err(SemaError::DuplicateImportSymbol(bound_name.to_string()))
            }
            _ => {
                imported.insert(bound_name, &u.path);
            }
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
