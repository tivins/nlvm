//! Native `system.*` class signatures — mirrors `nl_sema::stdlib` (kept
//! independent, matching this crate's existing pattern of not sharing
//! `class_table` with nl-sema either). See stdlib.md and vm.md § Standard
//! library binding: these classes have no `.nl` source and no backing
//! bytecode `Module` — the VM intercepts `INVOKE_STATIC` against them
//! directly (`nl_vm::native`), so nl-codegen only needs to emit a
//! `MethodRef` naming them, never a real class file.

use nl_syntax::ast::Type;

pub fn is_stdlib_class(fqcn: &str) -> bool {
    matches!(
        fqcn,
        "system.Out"
            | "system.Err"
            | "system.In"
            | "system.Int"
            | "system.Float"
            | "system.Bool"
            | "system.String"
            | "system.io.File"
            | "system.io.Directory"
            | "system.io.Path"
            | "system.SecureRandom"
            | "system.Uuid"
    )
}

fn file_handle() -> Type {
    Type::Named("system.io.FileHandle".to_string())
}

/// The one native class whose *instances* the user manipulates
/// (`system.io.File.open` returns one): its methods compile to an ordinary
/// `INVOKE_INSTANCE` (the VM intercepts by the receiver's runtime class,
/// `nl_vm::native::dispatch_native_instance`), with this table standing in
/// for the `ClassInfo` a bytecode-backed class would provide.
pub fn instance_signature(fqcn: &str, name: &str, argc: usize) -> Option<(Vec<Type>, Type)> {
    let nullable = |t: Type| Type::Union(vec![t, Type::NullT]);
    let byte_array = Type::Array(Box::new(Type::Byte));
    match (fqcn, name, argc) {
        ("system.io.FileHandle", "close", 0) => Some((vec![], Type::Void)),
        ("system.io.FileHandle", "read", 3) => Some((vec![byte_array, Type::Int, Type::Int], Type::Int)),
        ("system.io.FileHandle", "readLine", 0) => Some((vec![], nullable(Type::StringT))),
        ("system.io.FileHandle", "write", 1) => Some((vec![Type::StringT], Type::Void)),
        ("system.io.FileHandle", "write", 3) => Some((vec![byte_array, Type::Int, Type::Int], Type::Void)),
        ("system.io.FileHandle", "flush", 0) => Some((vec![], Type::Void)),
        ("system.Random", "nextInt", 0) => Some((vec![], Type::Int)),
        ("system.Random", "nextInt", 1) => Some((vec![Type::Int], Type::Int)),
        ("system.Random", "nextFloat", 0) => Some((vec![], Type::Float)),
        _ => None,
    }
}

/// Constructor parameter types for native instance classes constructible
/// via `new` directly (unlike `system.io.FileHandle`, only ever produced by
/// `File.open`) — consulted by `Emitter::compile_new` before falling back
/// to `class_table::find_ctor`, same precedence as
/// `native_generics::ctor_param_types`.
pub fn ctor_param_types(fqcn: &str, argc: usize) -> Option<Vec<Type>> {
    match (fqcn, argc) {
        ("system.Random", 0) => Some(vec![]),
        ("system.Random", 1) => Some(vec![Type::Int]),
        _ => None,
    }
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
///
/// `system.String` entries are keyed by the *total* argument count
/// including the receiver, since `text.trim()` (instance form,
/// `Emitter::compile_method_call`) and `system.String.trim(text)` (static
/// form, this function's normal caller `compile_stdlib_call`) both end up
/// emitting the exact same `INVOKE_STATIC system.String.trim(string)`
/// against `system.String` — stdlib.md documents them as equivalent. The
/// instance-call site prepends the already-compiled receiver's type before
/// looking up here, so both call shapes share this single table.
pub fn signature(fqcn: &str, name: &str, argc: usize) -> Option<(Vec<Type>, Type)> {
    let nullable = |t: Type| Type::Union(vec![t, Type::NullT]);
    let string_array = Type::Array(Box::new(Type::StringT));
    let byte_array = Type::Array(Box::new(Type::Byte));
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
        ("system.String", "length", 1) => Some((vec![Type::StringT], Type::Int)),
        ("system.String", "charAt", 2) => Some((vec![Type::StringT, Type::Int], Type::StringT)),
        ("system.String", "substring", 2) => Some((vec![Type::StringT, Type::Int], Type::StringT)),
        ("system.String", "substring", 3) => Some((vec![Type::StringT, Type::Int, Type::Int], Type::StringT)),
        ("system.String", "indexOf", 2) => Some((vec![Type::StringT, Type::StringT], Type::Int)),
        ("system.String", "indexOf", 3) => Some((vec![Type::StringT, Type::StringT, Type::Int], Type::Int)),
        ("system.String", "contains", 2) => Some((vec![Type::StringT, Type::StringT], Type::Bool)),
        ("system.String", "toUpperCase", 1) => Some((vec![Type::StringT], Type::StringT)),
        ("system.String", "toLowerCase", 1) => Some((vec![Type::StringT], Type::StringT)),
        ("system.String", "replace", 3) => Some((vec![Type::StringT, Type::StringT, Type::StringT], Type::StringT)),
        ("system.String", "startsWith", 2) => Some((vec![Type::StringT, Type::StringT], Type::Bool)),
        ("system.String", "endsWith", 2) => Some((vec![Type::StringT, Type::StringT], Type::Bool)),
        ("system.String", "trim", 1) => Some((vec![Type::StringT], Type::StringT)),
        ("system.String", "split", 2) => Some((vec![Type::StringT, Type::StringT], string_array)),
        ("system.io.File", "exists", 1) => Some((vec![Type::StringT], Type::Bool)),
        ("system.io.File", "open", 1) => Some((vec![Type::StringT], file_handle())),
        ("system.io.File", "readAllText", 1) => Some((vec![Type::StringT], Type::StringT)),
        ("system.io.File", "writeAllText", 2) => Some((vec![Type::StringT, Type::StringT], Type::Void)),
        ("system.io.Directory", "list", 1) => Some((vec![Type::StringT], string_array)),
        ("system.io.Directory", "create", 1) => Some((vec![Type::StringT], Type::Void)),
        ("system.io.Directory", "remove", 1) => Some((vec![Type::StringT], Type::Void)),
        ("system.io.Directory", "exists", 1) => Some((vec![Type::StringT], Type::Bool)),
        ("system.io.Path", "join", 1) => Some((vec![string_array], Type::StringT)),
        ("system.io.Path", "dirname", 1) => Some((vec![Type::StringT], Type::StringT)),
        ("system.io.Path", "basename", 1) => Some((vec![Type::StringT], Type::StringT)),
        ("system.io.Path", "extension", 1) => Some((vec![Type::StringT], nullable(Type::StringT))),
        ("system.io.Path", "normalize", 1) => Some((vec![Type::StringT], Type::StringT)),
        ("system.SecureRandom", "nextBytes", 1) => Some((vec![byte_array], Type::Void)),
        ("system.SecureRandom", "nextInt", 0) => Some((vec![], Type::Int)),
        ("system.SecureRandom", "nextInt", 1) => Some((vec![Type::Int], Type::Int)),
        ("system.Uuid", "random", 0) => Some((vec![], Type::StringT)),
        _ => None,
    }
}
