//! Native `system.*` class signatures — mirrors `nl_sema::stdlib` (kept
//! independent, matching this crate's existing pattern of not sharing
//! `class_table` with nl-sema either). See stdlib.md and vm.md § Standard
//! library binding: these classes have no `.nl` source and no backing
//! bytecode `Module` — the VM intercepts `INVOKE_STATIC` against them
//! directly (`nl_vm::native`), so nl-codegen only needs to emit a
//! `MethodRef` naming them, never a real class file.

use nl_syntax::ast::Type;

pub fn is_stdlib_class(fqcn: &str) -> bool {
    matches!(fqcn, "system.Out" | "system.Err" | "system.In" | "system.Int" | "system.Float" | "system.Bool")
}

/// `print`/`println` accept any of `int|float|bool|string` (stdlib.md:
/// "behave as if the value were converted to its string representation
/// first"). Rather than encode that as a union descriptor, nl-codegen
/// normalizes the argument with `ToString` when it isn't already a string
/// and always calls the single native `(string) -> void` overload — see
/// `Emitter::compile_stdlib_call`.
pub fn is_printlike(fqcn: &str, name: &str) -> bool {
    matches!(
        (fqcn, name),
        ("system.Out", "print") | ("system.Out", "println") | ("system.Err", "print") | ("system.Err", "println")
    )
}

/// `(param_types, return_type)` for every other stdlib method — used to
/// build both the call-site argument coercion and the native `MethodRef`'s
/// descriptor.
pub fn signature(fqcn: &str, name: &str, argc: usize) -> Option<(Vec<Type>, Type)> {
    let nullable = |t: Type| Type::Union(vec![t, Type::NullT]);
    match (fqcn, name, argc) {
        ("system.In", "readLine", 0) => Some((vec![], nullable(Type::StringT))),
        ("system.Int", "parse", 1) => Some((vec![Type::StringT], Type::Int)),
        ("system.Int", "tryParse", 1) => Some((vec![Type::StringT], nullable(Type::Int))),
        ("system.Int", "toString", 1) => Some((vec![Type::Int], Type::StringT)),
        ("system.Float", "parse", 1) => Some((vec![Type::StringT], Type::Float)),
        ("system.Float", "tryParse", 1) => Some((vec![Type::StringT], nullable(Type::Float))),
        ("system.Float", "toString", 1) => Some((vec![Type::Float], Type::StringT)),
        ("system.Bool", "parse", 1) => Some((vec![Type::StringT], Type::Bool)),
        ("system.Bool", "tryParse", 1) => Some((vec![Type::StringT], nullable(Type::Bool))),
        ("system.Bool", "toString", 1) => Some((vec![Type::Bool], Type::StringT)),
        _ => None,
    }
}
