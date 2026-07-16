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
            | "system.Env"
            | "system.net.TcpStream"
            | "system.net.Http"
            | "system.thread.Thread"
            | "system.ps.Process"
            | "system.text.Regex"
            | "system.text.Encoding"
            | "system.time.DateTime"
            | "system.time.TimeZone"
    )
}

fn file_handle() -> Type {
    Type::Named("system.io.FileHandle".to_string())
}

fn file_mode() -> Type {
    Type::Named("system.io.FileMode".to_string())
}

fn tcp_stream() -> Type {
    Type::Named("system.net.TcpStream".to_string())
}

fn http_response() -> Type {
    Type::Named("system.net.HttpResponse".to_string())
}

fn process_info() -> Type {
    Type::Named("system.ps.ProcessInfo".to_string())
}

/// `pub(crate)`, unlike this module's other type helpers, since
/// `Emitter::compile_stdlib_call`'s `system.ps.Process.run` special case
/// (see this file's `signature` doc comment) needs it directly rather than
/// through this table.
pub(crate) fn process_result() -> Type {
    Type::Named("system.ps.ProcessResult".to_string())
}

fn regex_match() -> Type {
    Type::Named("system.text.RegexMatch".to_string())
}

fn date_time() -> Type {
    Type::Named("system.time.DateTime".to_string())
}

fn time_zone() -> Type {
    Type::Named("system.time.TimeZone".to_string())
}

/// `system.io.FileMode.<name>` int constant, or `None` if unknown — mirrors
/// `nl_sema::stdlib::enum_const_ty`/`FILE_MODES` (same list, same order;
/// the position *is* the runtime tag `nl_vm::native`'s `File.open` switches
/// on). See that module's doc comment for why this is a constant rather
/// than a real enum.
pub fn enum_const_value(fqcn: &str, name: &str) -> Option<i64> {
    if fqcn != "system.io.FileMode" {
        return None;
    }
    ["Read", "Write", "Append", "ReadWrite", "ReadWriteTruncate", "ReadWriteAppend"]
        .iter()
        .position(|&m| m == name)
        .map(|i| i as i64)
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
        ("system.net.TcpListener", "accept", 0) => Some((vec![], tcp_stream())),
        ("system.net.TcpListener", "close", 0) => Some((vec![], Type::Void)),
        ("system.net.TcpStream", "read", 3) => Some((vec![byte_array.clone(), Type::Int, Type::Int], Type::Int)),
        ("system.net.TcpStream", "write", 3) => Some((vec![byte_array, Type::Int, Type::Int], Type::Void)),
        ("system.net.TcpStream", "close", 0) => Some((vec![], Type::Void)),
        ("system.net.UdpSocket", "bind", 2) => Some((vec![Type::StringT, Type::Int], Type::Void)),
        ("system.net.UdpSocket", "send", 3) => {
            Some((vec![Type::StringT, Type::Int, Type::Array(Box::new(Type::Byte))], Type::Void))
        }
        ("system.net.UdpSocket", "receive", 1) => Some((vec![Type::Array(Box::new(Type::Byte))], Type::Int)),
        ("system.net.UdpSocket", "close", 0) => Some((vec![], Type::Void)),
        ("system.thread.Thread", "start", 0) => Some((vec![], Type::Void)),
        ("system.thread.Thread", "join", 0) => Some((vec![], Type::Void)),
        ("system.thread.Thread", "join", 1) => Some((vec![Type::Int], Type::Bool)),
        ("system.thread.Thread", "isAlive", 0) => Some((vec![], Type::Bool)),
        ("system.thread.Mutex", "lock", 0) => Some((vec![], Type::Void)),
        ("system.thread.Mutex", "unlock", 0) => Some((vec![], Type::Void)),
        ("system.thread.Mutex", "tryLock", 0) => Some((vec![], Type::Bool)),
        ("system.thread.Semaphore", "acquire", 0) => Some((vec![], Type::Void)),
        ("system.thread.Semaphore", "release", 0) => Some((vec![], Type::Void)),
        ("system.thread.Semaphore", "tryAcquire", 0) => Some((vec![], Type::Bool)),
        ("system.time.DateTime", "getYear", 0) => Some((vec![], Type::Int)),
        ("system.time.DateTime", "getMonth", 0) => Some((vec![], Type::Int)),
        ("system.time.DateTime", "getDay", 0) => Some((vec![], Type::Int)),
        ("system.time.DateTime", "getHour", 0) => Some((vec![], Type::Int)),
        ("system.time.DateTime", "getMinute", 0) => Some((vec![], Type::Int)),
        ("system.time.DateTime", "getSecond", 0) => Some((vec![], Type::Int)),
        ("system.time.DateTime", "getTimeZone", 0) => Some((vec![], time_zone())),
        ("system.time.DateTime", "withTimeZone", 1) => Some((vec![time_zone()], date_time())),
        ("system.time.DateTime", "toUtc", 0) => Some((vec![], date_time())),
        ("system.time.DateTime", "format", 1) => Some((vec![Type::StringT], Type::StringT)),
        ("system.time.TimeZone", "getId", 0) => Some((vec![], Type::StringT)),
        ("system.time.TimeZone", "getOffsetMinutes", 1) => Some((vec![date_time()], Type::Int)),
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
        ("system.net.TcpListener", 2) => Some(vec![Type::StringT, Type::Int]),
        ("system.net.UdpSocket", 0) => Some(vec![]),
        // `Thread(() => void task)` — `Type::Void` is the same "no real
        // function type this phase" joker `Expr::Closure`'s own synthetic
        // type resolves to elsewhere (see `Emitter::coerce_value`'s
        // matching `ExprTy::Closure` branch, needed here for the first
        // call site that ever passes a closure as a call argument).
        ("system.thread.Thread", 1) => Some(vec![Type::Void]),
        ("system.thread.Mutex", 0) => Some(vec![]),
        ("system.thread.Semaphore", 1) => Some(vec![Type::Int]),
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
        ("system.io.File", "open", 2) => Some((vec![Type::StringT, file_mode()], file_handle())),
        ("system.io.File", "readAllText", 1) => Some((vec![Type::StringT], Type::StringT)),
        ("system.io.File", "writeAllText", 2) => Some((vec![Type::StringT, Type::StringT], Type::Void)),
        ("system.io.File", "glob", 2) => Some((vec![Type::StringT, Type::StringT], string_array)),
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
        // stdlib.md § system.Env — mirrors `nl_sema::stdlib::lookup`'s
        // matching entries.
        ("system.Env", "get", 1) => Some((vec![Type::StringT], nullable(Type::StringT))),
        ("system.Env", "set", 2) => Some((vec![Type::StringT, Type::StringT], Type::Void)),
        ("system.Env", "remove", 1) => Some((vec![Type::StringT], Type::Void)),
        ("system.Env", "list", 0) => Some((vec![], string_array)),
        ("system.net.TcpStream", "connect", 2) => Some((vec![Type::StringT, Type::Int], tcp_stream())),
        ("system.net.Http", "get", 1) => Some((vec![Type::StringT], http_response())),
        ("system.net.Http", "post", 2) => Some((vec![Type::StringT, Type::StringT], http_response())),
        ("system.thread.Thread", "sleep", 1) => Some((vec![Type::Int], Type::Void)),
        // `system.ps.Process.run` is deliberately absent here — its two
        // overloads (`string[] args` vs `string command`) share the same
        // arity, and unlike `system.Out.print`'s union of primitives, the
        // two shapes need genuinely different bytecode (no shared
        // normalization), so `compile_stdlib_call` special-cases it before
        // ever reaching this table. See `nl_sema::stdlib::lookup`'s matching
        // comment for why sema's table *can* just use a union type there.
        ("system.ps.Process", "list", 0) => Some((vec![], Type::Array(Box::new(process_info())))),
        ("system.ps.Process", "list", 1) => Some((vec![Type::Int], Type::Array(Box::new(process_info())))),
        ("system.ps.Process", "pid", 0) => Some((vec![], Type::Int)),
        ("system.ps.Process", "exit", 1) => Some((vec![Type::Int], Type::Void)),
        ("system.ps.Process", "getCwd", 0) => Some((vec![], Type::StringT)),
        ("system.ps.Process", "setCwd", 1) => Some((vec![Type::StringT], Type::Void)),
        // stdlib.md § system.text.Regex/system.text.Encoding.
        ("system.text.Regex", "match", 2) => Some((vec![Type::StringT, Type::StringT], Type::Bool)),
        ("system.text.Regex", "matchFirst", 2) => Some((vec![Type::StringT, Type::StringT], nullable(regex_match()))),
        ("system.text.Regex", "replace", 3) => {
            Some((vec![Type::StringT, Type::StringT, Type::StringT], Type::StringT))
        }
        ("system.text.Regex", "split", 2) => Some((vec![Type::StringT, Type::StringT], string_array)),
        ("system.text.Regex", "escape", 1) => Some((vec![Type::StringT], Type::StringT)),
        ("system.text.Encoding", "encodeUtf8", 1) => Some((vec![Type::StringT], byte_array.clone())),
        ("system.text.Encoding", "decodeUtf8", 1) => Some((vec![byte_array.clone()], Type::StringT)),
        ("system.text.Encoding", "base64Encode", 1) => Some((vec![byte_array], Type::StringT)),
        ("system.text.Encoding", "base64Decode", 1) => {
            Some((vec![Type::StringT], Type::Array(Box::new(Type::Byte))))
        }
        // stdlib.md § system.time.DateTime/TimeZone — mirrors
        // `nl_sema::stdlib::lookup`'s matching entries.
        ("system.time.DateTime", "now", 0) => Some((vec![], date_time())),
        ("system.time.DateTime", "now", 1) => Some((vec![time_zone()], date_time())),
        ("system.time.DateTime", "parse", 1) => Some((vec![Type::StringT], date_time())),
        ("system.time.TimeZone", "getDefault", 0) => Some((vec![], time_zone())),
        ("system.time.TimeZone", "get", 1) => Some((vec![Type::StringT], time_zone())),
        _ => None,
    }
}

/// `system.net.HttpResponse`'s public fields — mirrors
/// `nl_sema::stdlib::result_field_ty` (same non-generic native result type
/// as `system.MapEntry<K,V>` but without a mangled name to parse types
/// out of, so it gets its own small table instead of going through
/// `native_generics::field_ty`).
pub fn result_field_ty(fqcn: &str, name: &str) -> Option<Type> {
    let nullable = |t: Type| Type::Union(vec![t, Type::NullT]);
    match (fqcn, name) {
        ("system.net.HttpResponse", "statusCode") => Some(Type::Int),
        ("system.net.HttpResponse", "body") => Some(Type::StringT),
        ("system.net.HttpResponse", "headers") => Some(nullable(Type::Array(Box::new(Type::StringT)))),
        ("system.ps.ProcessInfo", "pid") => Some(Type::Int),
        ("system.ps.ProcessInfo", "command") => Some(Type::StringT),
        ("system.ps.ProcessInfo", "args") => Some(Type::Array(Box::new(Type::StringT))),
        ("system.ps.ProcessInfo", "user") => Some(nullable(Type::StringT)),
        ("system.ps.ProcessResult", "exitCode") => Some(Type::Int),
        ("system.ps.ProcessResult", "stdout") => Some(Type::StringT),
        ("system.ps.ProcessResult", "stderr") => Some(Type::StringT),
        ("system.text.RegexMatch", "fullMatch") => Some(Type::StringT),
        ("system.text.RegexMatch", "groups") => Some(Type::Array(Box::new(Type::StringT))),
        _ => None,
    }
}
