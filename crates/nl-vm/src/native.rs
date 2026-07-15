//! Native bindings for the `system.*` stdlib classes — vm.md § Standard
//! library binding: "calling `system.Out.print(s)` is an `INVOKE_STATIC`
//! like any other — the VM intercepts the call and runs the native code."
//! `interpreter::exec_step`'s `INVOKE_STATIC` arm calls `dispatch` for any
//! class name `is_native_class` accepts, before ever consulting `Program`'s
//! module map (these classes have no backing bytecode `Module` — see
//! `nl_codegen::stdlib`/`nl_sema::stdlib`, which are what type-check and
//! emit calls against them).
//!
//! Only part of stdlib.md is covered so far (PLAN.md Phase 6): output
//! (`system.Out`/`system.Err`), `system.In.readLine`, int/float/bool
//! parsing/formatting, `system.String` (instance methods on `string`
//! values and their static equivalents — both compile to the same
//! `INVOKE_STATIC system.String.<name>`, see `nl_codegen::stdlib`), and
//! `system.List<T>`/`system.Map<K,V>` (see the section below). File I/O,
//! threads, etc. are future work.
//!
//! ## `system.List<T>` / `system.Map<K,V>`
//!
//! Unlike every native class above, these are real heap objects created
//! with `new` (vm.md § Templates (monomorphization) — "native template
//! classes"), not static-only utility classes. `interpreter::exec_step`
//! intercepts all three opcodes that touch them — `NEW` (`new_generic_object`
//! instead of the usual module-based field walk), `INVOKE_SPECIAL` on
//! `<construct>` (`construct_generic`), and `INVOKE_INSTANCE` keyed by the
//! *receiver's* runtime class (`dispatch_instance`) — via
//! `is_native_generic_class`, mirroring how `is_native_class` intercepts
//! `INVOKE_STATIC` for the utility classes. `nl_sema`/`nl_codegen`'s
//! `native_generics` modules recover each instantiation's concrete element
//! type(s) by parsing the mangled FQCN (e.g. `"system.Map<string, int>"`);
//! this module only needs the *values*, not their static types, so it
//! doesn't need that parsing.
//!
//! Representation: a `List<T>` instance is a plain `Value::Object` with one
//! field, `"__data__"`, holding the backing `Value::Array`. A `Map<K,V>`
//! instance has two parallel array fields, `"__keys__"`/`"__values__"` (same
//! index in both = one entry) — chosen over a real hash map because key
//! equality follows `values_equal` (§ below), which isn't `Hash`-compatible
//! in general (e.g. float, or reference-identity for plain objects), and
//! map sizes in test programs are small enough that O(n) lookup is not a
//! concern. `keys()`/`values()` return a *copy* of the backing array (a
//! fresh `Rc`), not a live view — mutating the returned array must not
//! desync it from the map, per stdlib.md's "Returns an array containing".
//!
//! Key/element equality for `contains`/map lookups reuses
//! `interpreter::values_equal` (primitives and `string` by value,
//! everything else by reference identity) — this is the same rule
//! stdlib.md documents as the *fallback* for types that don't implement
//! `ValueEquatable`; `ValueEquatable` itself is not implemented, so that
//! optimization never kicks in (reference types with structural key/element
//! equality always fall back to identity here).
//!
//! Not implemented (PLAN.md Phase 6 gap): `system.List`'s `T[] initial`
//! constructor works, but `entries()`/`forEach` on `Map` do not (they need
//! a synthetic `MapEntry<K,V>` class and closures-as-native-callbacks,
//! neither of which exist yet), and neither collection supports the
//! for-each loop (`for (const auto x : list)`) — vm.md's desugaring for
//! that relies on `entries()` for maps and hasn't been wired into
//! nl-codegen for either collection.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::error::VmError;
use crate::interpreter::values_equal;
use crate::program::Program;
use crate::value::{Object, Value};

pub fn is_native_class(fqcn: &str) -> bool {
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

/// Dispatches one native call. `args` has already been popped off the
/// operand stack by the caller, in declaration order. Returns `Ok(None)`
/// for a `void` native (nothing to push back).
pub fn dispatch(program: &Program, fqcn: &str, name: &str, mut args: Vec<Value>) -> Result<Option<Value>, VmError> {
    match (fqcn, name) {
        ("system.Out", "print") => {
            program.write_stdout(&expect_str(&mut args)?);
            Ok(None)
        }
        ("system.Out", "println") => {
            let mut s = expect_str(&mut args)?;
            s.push('\n');
            program.write_stdout(&s);
            Ok(None)
        }
        ("system.Err", "print") => {
            program.write_stderr(&expect_str(&mut args)?);
            Ok(None)
        }
        ("system.Err", "println") => {
            let mut s = expect_str(&mut args)?;
            s.push('\n');
            program.write_stderr(&s);
            Ok(None)
        }
        ("system.In", "readLine") => {
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) => Ok(Some(Value::Null)), // EOF
                Ok(_) => {
                    if line.ends_with('\n') {
                        line.pop();
                        if line.ends_with('\r') {
                            line.pop();
                        }
                    }
                    Ok(Some(Value::Str(Rc::new(line))))
                }
                Err(e) => Err(VmError::Io(e)),
            }
        }
        ("system.Int", "parse") => match expect_str(&mut args)?.trim().parse::<i64>() {
            Ok(v) => Ok(Some(Value::Int(v))),
            Err(_) => Err(throw_format_error("invalid int literal")),
        },
        ("system.Int", "tryParse") => match expect_str(&mut args)?.trim().parse::<i64>() {
            Ok(v) => Ok(Some(Value::Int(v))),
            Err(_) => Ok(Some(Value::Null)),
        },
        ("system.Int", "toString") => Ok(Some(Value::Str(Rc::new(expect_int(&mut args)?.to_string())))),
        ("system.Float", "parse") => match expect_str(&mut args)?.trim().parse::<f64>() {
            Ok(v) => Ok(Some(Value::Float(v))),
            Err(_) => Err(throw_format_error("invalid float literal")),
        },
        ("system.Float", "tryParse") => match expect_str(&mut args)?.trim().parse::<f64>() {
            Ok(v) => Ok(Some(Value::Float(v))),
            Err(_) => Ok(Some(Value::Null)),
        },
        ("system.Float", "toString") => Ok(Some(Value::Str(Rc::new(expect_float(&mut args)?.to_string())))),
        ("system.Bool", "parse") => match expect_str(&mut args)?.as_str() {
            "true" => Ok(Some(Value::Bool(true))),
            "false" => Ok(Some(Value::Bool(false))),
            _ => Err(throw_native("IllegalArgumentException", "expected \"true\" or \"false\"")),
        },
        ("system.Bool", "tryParse") => match expect_str(&mut args)?.as_str() {
            "true" => Ok(Some(Value::Bool(true))),
            "false" => Ok(Some(Value::Bool(false))),
            _ => Ok(Some(Value::Null)),
        },
        ("system.Bool", "toString") => Ok(Some(Value::Str(Rc::new(expect_bool(&mut args)?.to_string())))),
        // stdlib.md § system.String — `args[0]` is always the receiver
        // (whether the call came from `text.trim()` or the equivalent
        // static `system.String.trim(text)`, see nl_codegen::stdlib's doc
        // comment); indexed rather than popped since several of these take
        // more than one argument and popping would read them back to
        // front. Character positions are counted in `char`s, not bytes
        // (specs.md: "A character is represented as a string of length
        // 1").
        ("system.String", "length") => Ok(Some(Value::Int(str_at(&args, 0)?.chars().count() as i64))),
        ("system.String", "charAt") => {
            let chars: Vec<char> = str_at(&args, 0)?.chars().collect();
            let idx = int_at(&args, 1)?;
            if idx < 0 || idx as usize >= chars.len() {
                return Err(throw_native("IndexOutOfBoundsException", format!("index {idx}, length {}", chars.len())));
            }
            Ok(Some(Value::Str(Rc::new(chars[idx as usize].to_string()))))
        }
        ("system.String", "substring") => {
            let chars: Vec<char> = str_at(&args, 0)?.chars().collect();
            let start = int_at(&args, 1)?;
            let end = if args.len() >= 3 { int_at(&args, 2)? } else { chars.len() as i64 };
            if start < 0 || end < start || end as usize > chars.len() {
                return Err(throw_native(
                    "IndexOutOfBoundsException",
                    format!("start {start}, end {end}, length {}", chars.len()),
                ));
            }
            let sub: String = chars[start as usize..end as usize].iter().collect();
            Ok(Some(Value::Str(Rc::new(sub))))
        }
        ("system.String", "indexOf") => {
            let haystack = str_at(&args, 0)?;
            let needle = str_at(&args, 1)?;
            let from = if args.len() >= 3 { int_at(&args, 2)?.max(0) as usize } else { 0 };
            Ok(Some(Value::Int(char_index_of(&haystack, &needle, from).unwrap_or(-1))))
        }
        ("system.String", "contains") => Ok(Some(Value::Bool(str_at(&args, 0)?.contains(&str_at(&args, 1)?)))),
        ("system.String", "toUpperCase") => Ok(Some(Value::Str(Rc::new(str_at(&args, 0)?.to_uppercase())))),
        ("system.String", "toLowerCase") => Ok(Some(Value::Str(Rc::new(str_at(&args, 0)?.to_lowercase())))),
        ("system.String", "replace") => {
            let s = str_at(&args, 0)?;
            let from = str_at(&args, 1)?;
            let to = str_at(&args, 2)?;
            Ok(Some(Value::Str(Rc::new(s.replace(&from, &to)))))
        }
        ("system.String", "startsWith") => Ok(Some(Value::Bool(str_at(&args, 0)?.starts_with(&str_at(&args, 1)?)))),
        ("system.String", "endsWith") => Ok(Some(Value::Bool(str_at(&args, 0)?.ends_with(&str_at(&args, 1)?)))),
        ("system.String", "trim") => Ok(Some(Value::Str(Rc::new(str_at(&args, 0)?.trim().to_string())))),
        ("system.String", "split") => {
            let s = str_at(&args, 0)?;
            let delim = str_at(&args, 1)?;
            let parts: Vec<Value> = s.split(delim.as_str()).map(|p| Value::Str(Rc::new(p.to_string()))).collect();
            Ok(Some(Value::Array(Rc::new(RefCell::new(parts)))))
        }
        // stdlib.md § system.io.File — paths are used as-is, no
        // sanitization ("path validation is the caller's responsibility").
        ("system.io.File", "exists") => Ok(Some(Value::Bool(std::path::Path::new(&str_at(&args, 0)?).exists()))),
        ("system.io.File", "open") => {
            let path = str_at(&args, 0)?;
            // 1-argument open == `FileMode.ReadWrite` (stdlib.md): read and
            // write, file must already exist, positioned at the start.
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| throw_io_error(&path, e))?;
            let id = program.register_file(file);
            let mut fields = HashMap::new();
            fields.insert("__fd__".to_string(), Value::Int(id));
            Ok(Some(Value::Object(Rc::new(RefCell::new(Object {
                class_name: "system.io.FileHandle".to_string(),
                fields,
            })))))
        }
        ("system.io.File", "readAllText") => {
            let path = str_at(&args, 0)?;
            let text = std::fs::read_to_string(&path).map_err(|e| throw_io_error(&path, e))?;
            Ok(Some(Value::Str(Rc::new(text))))
        }
        ("system.io.File", "writeAllText") => {
            let path = str_at(&args, 0)?;
            let content = str_at(&args, 1)?;
            std::fs::write(&path, content).map_err(|e| throw_io_error(&path, e))?;
            Ok(None)
        }
        ("system.io.Directory", "list") => {
            let path = str_at(&args, 0)?;
            let entries = std::fs::read_dir(&path).map_err(|e| throw_io_error(&path, e))?;
            let mut names = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|e| throw_io_error(&path, e))?;
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
            // read_dir order is platform-dependent; sorted so NL programs
            // (and their expected_stdout in tests) see a stable order.
            names.sort();
            let values = names.into_iter().map(|n| Value::Str(Rc::new(n))).collect();
            Ok(Some(Value::Array(Rc::new(RefCell::new(values)))))
        }
        ("system.io.Directory", "create") => {
            let path = str_at(&args, 0)?;
            std::fs::create_dir_all(&path).map_err(|e| throw_io_error(&path, e))?;
            Ok(None)
        }
        ("system.io.Directory", "remove") => {
            let path = str_at(&args, 0)?;
            std::fs::remove_dir(&path).map_err(|e| throw_io_error(&path, e))?;
            Ok(None)
        }
        ("system.io.Directory", "exists") => Ok(Some(Value::Bool(std::path::Path::new(&str_at(&args, 0)?).is_dir()))),
        ("system.io.Path", "join") => {
            let Some(Value::Array(segments)) = args.first() else {
                return Err(VmError::Malformed("expected string[] argument to native call"));
            };
            let mut joined = std::path::PathBuf::new();
            for seg in segments.borrow().iter() {
                let Value::Str(s) = seg else {
                    return Err(VmError::Malformed("expected string[] argument to native call"));
                };
                joined.push(s.as_str());
            }
            Ok(Some(Value::Str(Rc::new(joined.to_string_lossy().into_owned()))))
        }
        ("system.io.Path", "dirname") => {
            let path = str_at(&args, 0)?;
            let dir = std::path::Path::new(&path).parent().map(|p| p.to_string_lossy().into_owned());
            Ok(Some(Value::Str(Rc::new(dir.unwrap_or_default()))))
        }
        ("system.io.Path", "basename") => {
            let path = str_at(&args, 0)?;
            let base = std::path::Path::new(&path).file_name().map(|p| p.to_string_lossy().into_owned());
            Ok(Some(Value::Str(Rc::new(base.unwrap_or_default()))))
        }
        ("system.io.Path", "extension") => {
            let path = str_at(&args, 0)?;
            // stdlib.md: "Returns the file extension (e.g. `.nl`)" — with
            // the leading dot, or null when there is none.
            match std::path::Path::new(&path).extension() {
                Some(ext) => Ok(Some(Value::Str(Rc::new(format!(".{}", ext.to_string_lossy()))))),
                None => Ok(Some(Value::Null)),
            }
        }
        ("system.io.Path", "normalize") => Ok(Some(Value::Str(Rc::new(normalize_path(&str_at(&args, 0)?))))),
        // stdlib.md § system.SecureRandom — CSPRNG backed by `/dev/urandom`,
        // same source `system.Uuid.random` uses. Not seedable.
        ("system.SecureRandom", "nextBytes") => {
            let Some(Value::Array(buffer)) = args.first().cloned() else {
                return Err(VmError::Malformed("expected byte[] argument to native call"));
            };
            let len = buffer.borrow().len();
            let random = secure_random_bytes(len)?;
            let mut buf = buffer.borrow_mut();
            for (slot, b) in buf.iter_mut().zip(random) {
                *slot = Value::Byte(b);
            }
            Ok(None)
        }
        ("system.SecureRandom", "nextInt") => {
            if args.is_empty() {
                Ok(Some(Value::Int(secure_next_u64()? as i64)))
            } else {
                let bound = expect_int(&mut args)?;
                Ok(Some(Value::Int(secure_bounded_int(bound)?)))
            }
        }
        // stdlib.md § system.Uuid — UUID v4, 122 random bits from the same
        // CSPRNG as SecureRandom, version/variant nibbles set per RFC 4122.
        ("system.Uuid", "random") => Ok(Some(Value::Str(Rc::new(uuid_v4()?)))),
        _ => Err(VmError::MethodNotFound(format!("{fqcn}.{name}"))),
    }
}

/// Reads `n` cryptographically secure random bytes from the OS entropy
/// source (stdlib.md names `/dev/urandom` explicitly as an example backing
/// source for `system.SecureRandom`).
fn secure_random_bytes(n: usize) -> Result<Vec<u8>, VmError> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").map_err(VmError::Io)?;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf).map_err(VmError::Io)?;
    Ok(buf)
}

fn secure_next_u64() -> Result<u64, VmError> {
    let bytes = secure_random_bytes(8)?;
    Ok(u64::from_le_bytes(bytes.try_into().expect("8 bytes")))
}

/// Rejection sampling so every result in `[0, bound)` is equally likely
/// (stdlib.md: "uniformly distributed (no modulo bias)") — a plain modulo
/// would slightly favor small results whenever `u64::MAX + 1` isn't a
/// multiple of `bound`.
fn secure_bounded_int(bound: i64) -> Result<i64, VmError> {
    if bound <= 0 {
        return Err(throw_native("IllegalArgumentException", "bound must be positive"));
    }
    let bound_u = bound as u64;
    let limit = u64::MAX - (u64::MAX % bound_u);
    loop {
        let r = secure_next_u64()?;
        if r < limit {
            return Ok((r % bound_u) as i64);
        }
    }
}

fn uuid_v4() -> Result<String, VmError> {
    let mut b = secure_random_bytes(16)?;
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
    ))
}

/// Purely lexical `.`/`..`/redundant-separator resolution (stdlib.md §
/// system.io.Path — `normalize` is documented as the *pre-I/O* validation
/// step for untrusted paths, so it must not touch the file system the way
/// `std::fs::canonicalize` would). A `..` at the start (nothing left to pop)
/// is kept as-is, matching the usual lexical-normalization convention.
fn normalize_path(path: &str) -> String {
    use std::path::Component;
    let mut parts: Vec<String> = Vec::new();
    let mut absolute = false;
    for component in std::path::Path::new(path).components() {
        match component {
            Component::RootDir => absolute = true,
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.last().is_some_and(|p| p != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..".to_string());
                }
            }
            other => parts.push(other.as_os_str().to_string_lossy().into_owned()),
        }
    }
    let joined = parts.join(std::path::MAIN_SEPARATOR_STR);
    match (absolute, joined.is_empty()) {
        (true, _) => format!("{}{joined}", std::path::MAIN_SEPARATOR),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

/// Maps a host I/O error to the spec's exception types — stdlib.md:
/// `FileNotFoundException` when the path does not exist, `IOException` for
/// every other failure.
fn throw_io_error(path: &str, err: std::io::Error) -> VmError {
    match err.kind() {
        std::io::ErrorKind::NotFound => throw_native("FileNotFoundException", format!("{path}: {err}")),
        _ => throw_native("IOException", format!("{path}: {err}")),
    }
}

fn str_at(args: &[Value], i: usize) -> Result<String, VmError> {
    match args.get(i) {
        Some(Value::Str(s)) => Ok((**s).clone()),
        _ => Err(VmError::Malformed("expected string argument to native call")),
    }
}

fn int_at(args: &[Value], i: usize) -> Result<i64, VmError> {
    args.get(i).and_then(|v| v.as_int()).ok_or(VmError::Malformed("expected int argument to native call"))
}

/// Char-index (not byte-index) of the first occurrence of `needle` in
/// `haystack` at or after char position `from`, or `None`. An empty
/// `needle` matches at `from` itself, mirroring `str::find`'s behavior.
fn char_index_of(haystack: &str, needle: &str, from: usize) -> Option<i64> {
    let hay: Vec<char> = haystack.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() {
        return if from <= hay.len() { Some(from as i64) } else { None };
    }
    if from > hay.len() || needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&start| hay[start..start + needle.len()] == needle[..]).map(|s| s as i64)
}

fn expect_str(args: &mut Vec<Value>) -> Result<String, VmError> {
    match args.pop() {
        Some(Value::Str(s)) => Ok((*s).clone()),
        _ => Err(VmError::Malformed("expected string argument to native call")),
    }
}

fn expect_int(args: &mut Vec<Value>) -> Result<i64, VmError> {
    args.pop().and_then(|v| v.as_int()).ok_or(VmError::Malformed("expected int argument to native call"))
}

fn expect_float(args: &mut Vec<Value>) -> Result<f64, VmError> {
    args.pop().and_then(|v| v.as_float()).ok_or(VmError::Malformed("expected float argument to native call"))
}

fn expect_bool(args: &mut Vec<Value>) -> Result<bool, VmError> {
    args.pop().and_then(|v| v.as_bool()).ok_or(VmError::Malformed("expected bool argument to native call"))
}

fn throw_format_error(message: impl Into<String>) -> VmError {
    throw_native("NumberFormatException", message)
}

fn throw_native(class_name: &str, message: impl Into<String>) -> VmError {
    let mut fields = HashMap::new();
    fields.insert("message".to_string(), Value::Str(Rc::new(message.into())));
    VmError::Thrown(Value::Object(Rc::new(RefCell::new(crate::value::Object {
        class_name: class_name.to_string(),
        fields,
    }))))
}

/// `system.io.FileHandle` and `system.Random` — like the native generic
/// collections below, real heap objects dispatched through
/// `INVOKE_INSTANCE` on their runtime class. `FileHandle` is stateful
/// *outside* the object (an `"__fd__"` index into `Program::file_handles`,
/// which is why `dispatch_native_instance` takes `program`); `Random`
/// instead keeps its PRNG state directly on the object (`"__state__"`, see
/// `is_random_class`/`dispatch_random` below) and ignores `program`
/// entirely.
pub fn is_native_instance_class(fqcn: &str) -> bool {
    matches!(fqcn, "system.io.FileHandle" | "system.Random")
}

pub fn dispatch_native_instance(
    program: &Program,
    name: &str,
    receiver: &Value,
    args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    use std::io::{Read, Write};

    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected native instance receiver"));
    };
    if obj.borrow().class_name == "system.Random" {
        return dispatch_random(name, receiver, args);
    }
    let id = match obj.borrow().fields.get("__fd__") {
        Some(Value::Int(id)) => *id,
        _ => return Err(VmError::Malformed("malformed FileHandle object")),
    };

    // `close()` is idempotent and never fails (stdlib.md); everything else
    // on a closed handle throws IOException — including `m7_0030`'s
    // read-after-close (CWE-416) scenario.
    if name == "close" {
        program.close_file(id);
        return Ok(None);
    }
    let closed = || throw_native("IOException", format!("{name} on a closed file handle"));

    match name {
        "read" | "write" if args.len() == 3 => {
            let Some(Value::Array(buffer)) = args.first().cloned() else {
                return Err(VmError::Malformed("expected byte[] argument to native call"));
            };
            let offset = int_at(&args, 1)?;
            let length = int_at(&args, 2)?;
            let buf_len = buffer.borrow().len() as i64;
            // stdlib.md § system.io.FileHandle, Bounds checking: checked
            // *before any I/O*, immune to `offset + length` overflow
            // (checked_add instead of wrapping `+`).
            if offset < 0 || length < 0 || offset.checked_add(length).is_none_or(|end| end > buf_len) {
                return Err(throw_native(
                    "IndexOutOfBoundsException",
                    format!("offset {offset}, length {length}, buffer length {buf_len}"),
                ));
            }
            if name == "read" {
                let mut tmp = vec![0u8; length as usize];
                let n = program
                    .with_file(id, |f| f.read(&mut tmp))
                    .ok_or_else(closed)?
                    .map_err(|e| throw_native("IOException", e.to_string()))?;
                let mut buf = buffer.borrow_mut();
                for (i, byte) in tmp[..n].iter().enumerate() {
                    buf[offset as usize + i] = Value::Byte(*byte);
                }
                Ok(Some(Value::Int(n as i64)))
            } else {
                let data: Vec<u8> = buffer.borrow()[offset as usize..(offset + length) as usize]
                    .iter()
                    .map(|v| match v {
                        Value::Byte(b) => Ok(*b),
                        // `int` stored through a `byte[]` element keeps the
                        // low-order bits, same as the `(byte)` cast rule.
                        Value::Int(i) => Ok(*i as u8),
                        _ => Err(VmError::Malformed("expected byte[] argument to native call")),
                    })
                    .collect::<Result<_, _>>()?;
                program
                    .with_file(id, |f| f.write_all(&data))
                    .ok_or_else(closed)?
                    .map_err(|e| throw_native("IOException", e.to_string()))?;
                Ok(None)
            }
        }
        "write" if args.len() == 1 => {
            let text = str_at(&args, 0)?;
            program
                .with_file(id, |f| f.write_all(text.as_bytes()))
                .ok_or_else(closed)?
                .map_err(|e| throw_native("IOException", e.to_string()))?;
            Ok(None)
        }
        "readLine" => {
            // Byte-at-a-time keeps the OS file position exactly after the
            // `\n` (a `BufReader` would read ahead and desync the handle
            // for the interleaved `read`/`write` calls stdlib.md allows).
            let line = program
                .with_file(id, |f| -> std::io::Result<Option<String>> {
                    let mut bytes = Vec::new();
                    let mut one = [0u8; 1];
                    loop {
                        if f.read(&mut one)? == 0 {
                            // EOF: null if nothing was read at all.
                            return Ok(if bytes.is_empty() { None } else { Some(lossy_line(bytes)) });
                        }
                        if one[0] == b'\n' {
                            return Ok(Some(lossy_line(bytes)));
                        }
                        bytes.push(one[0]);
                    }
                })
                .ok_or_else(closed)?
                .map_err(|e| throw_native("IOException", e.to_string()))?;
            Ok(Some(match line {
                Some(l) => Value::Str(Rc::new(l)),
                None => Value::Null,
            }))
        }
        "flush" => {
            program
                .with_file(id, |f| f.flush())
                .ok_or_else(closed)?
                .map_err(|e| throw_native("IOException", e.to_string()))?;
            Ok(None)
        }
        _ => Err(VmError::MethodNotFound(format!("system.io.FileHandle.{name}"))),
    }
}

/// One decoded `readLine` result: UTF-8 (lossy) with a trailing `\r`
/// stripped, mirroring `system.In.readLine`'s CRLF handling.
fn lossy_line(bytes: Vec<u8>) -> String {
    let mut s = String::from_utf8_lossy(&bytes).into_owned();
    if s.ends_with('\r') {
        s.pop();
    }
    s
}

/// stdlib.md § system.Random — a deterministic PRNG (SplitMix64), seeded
/// either explicitly (`construct(int seed)`, reproducible) or from an
/// implementation-defined default (`construct()`, mixing wall-clock time
/// with a process-wide counter so back-to-back default constructions still
/// diverge). Unlike the native generic collections, `fqcn` here is never
/// mangled (`"system.Random"`, no type arguments), so `NEW`/`INVOKE_SPECIAL`
/// intercept it by exact name (`is_random_class`) rather than a prefix
/// check.
pub fn is_random_class(fqcn: &str) -> bool {
    fqcn == "system.Random"
}

pub fn new_random_object() -> Value {
    let mut fields = HashMap::new();
    fields.insert("__state__".to_string(), Value::Int(0));
    Value::Object(Rc::new(RefCell::new(Object {
        class_name: "system.Random".to_string(),
        fields,
    })))
}

pub fn construct_random(receiver: &Value, mut args: Vec<Value>) -> Result<(), VmError> {
    let seed = match args.pop() {
        Some(v) => v.as_int().ok_or(VmError::Malformed("expected int seed argument to native call"))? as u64,
        None => default_random_seed(),
    };
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected Random receiver"));
    };
    obj.borrow_mut().fields.insert("__state__".to_string(), Value::Int(seed as i64));
    Ok(())
}

fn default_random_seed() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    nanos ^ count.wrapping_mul(0x9E3779B97F4A7C15)
}

/// SplitMix64 (Steele, Lea & Flood) — small, fast, and statistically solid
/// for a non-cryptographic PRNG; advances `state` in place and returns the
/// next 64-bit output.
fn splitmix64_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

fn dispatch_random(name: &str, receiver: &Value, mut args: Vec<Value>) -> Result<Option<Value>, VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected Random receiver"));
    };
    let mut state = match obj.borrow().fields.get("__state__") {
        Some(Value::Int(s)) => *s as u64,
        _ => return Err(VmError::Malformed("malformed Random object")),
    };
    let result = match name {
        "nextInt" if args.is_empty() => Value::Int(splitmix64_next(&mut state) as i64),
        "nextInt" => {
            let bound = expect_int(&mut args)?;
            if bound <= 0 {
                return Err(throw_native("IllegalArgumentException", "bound must be positive"));
            }
            Value::Int((splitmix64_next(&mut state) % bound as u64) as i64)
        }
        "nextFloat" => {
            let raw = splitmix64_next(&mut state) >> 11; // top 53 bits
            Value::Float(raw as f64 * (1.0 / (1u64 << 53) as f64))
        }
        _ => return Err(VmError::MethodNotFound(format!("system.Random.{name}"))),
    };
    obj.borrow_mut().fields.insert("__state__".to_string(), Value::Int(state as i64));
    Ok(Some(result))
}

pub fn is_native_generic_class(fqcn: &str) -> bool {
    fqcn.starts_with("system.List<") || fqcn.starts_with("system.Map<")
}

/// `Opcode::New` against a native generic class — see this module's doc
/// comment for the field layout. Both collections start out empty; a
/// `List<T>(T[] initial)` constructor call fills `__data__` afterwards via
/// `construct_generic`.
pub fn new_generic_object(fqcn: &str) -> Value {
    let mut fields = HashMap::new();
    if fqcn.starts_with("system.List<") {
        fields.insert("__data__".to_string(), Value::Array(Rc::new(RefCell::new(Vec::new()))));
    } else {
        fields.insert("__keys__".to_string(), Value::Array(Rc::new(RefCell::new(Vec::new()))));
        fields.insert("__values__".to_string(), Value::Array(Rc::new(RefCell::new(Vec::new()))));
    }
    Value::Object(Rc::new(RefCell::new(Object { class_name: fqcn.to_string(), fields })))
}

/// `Opcode::InvokeSpecial` on a native generic class's `<construct>`. Only
/// `system.List<T>(T[] initial)` does anything; `List()` and `Map()` leave
/// the empty fields `new_generic_object` already set up untouched.
pub fn construct_generic(receiver: &Value, fqcn: &str, mut args: Vec<Value>) -> Result<(), VmError> {
    if fqcn.starts_with("system.List<") {
        if let Some(Value::Array(initial)) = args.pop() {
            list_data(receiver)?.borrow_mut().extend(initial.borrow().iter().cloned());
        }
    }
    Ok(())
}

/// `Opcode::InvokeInstance` against a native generic class — dispatched by
/// the *receiver's* runtime class, same as `resolve_virtual` would for a
/// bytecode-backed class.
pub fn dispatch_instance(fqcn: &str, name: &str, receiver: &Value, args: Vec<Value>) -> Result<Option<Value>, VmError> {
    if fqcn.starts_with("system.List<") {
        dispatch_list(name, receiver, args)
    } else {
        dispatch_map(name, receiver, args)
    }
}

type ArrayRc = Rc<RefCell<Vec<Value>>>;

fn list_data(receiver: &Value) -> Result<ArrayRc, VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected List receiver"));
    };
    match obj.borrow().fields.get("__data__") {
        Some(Value::Array(a)) => Ok(Rc::clone(a)),
        _ => Err(VmError::Malformed("malformed List object")),
    }
}

fn dispatch_list(name: &str, receiver: &Value, mut args: Vec<Value>) -> Result<Option<Value>, VmError> {
    let data = list_data(receiver)?;
    match name {
        "size" => Ok(Some(Value::Int(data.borrow().len() as i64))),
        "get" => {
            let idx = expect_int(&mut args)?;
            let d = data.borrow();
            if idx < 0 || idx as usize >= d.len() {
                return Err(throw_native("IndexOutOfBoundsException", format!("index {idx}, length {}", d.len())));
            }
            Ok(Some(d[idx as usize].clone()))
        }
        "set" => {
            let value = args.pop().ok_or(VmError::Malformed("missing value argument"))?;
            let idx = expect_int(&mut args)?;
            let mut d = data.borrow_mut();
            if idx < 0 || idx as usize >= d.len() {
                return Err(throw_native("IndexOutOfBoundsException", format!("index {idx}, length {}", d.len())));
            }
            d[idx as usize] = value;
            Ok(None)
        }
        "pushBack" | "add" => {
            let value = args.pop().ok_or(VmError::Malformed("missing value argument"))?;
            data.borrow_mut().push(value);
            Ok(None)
        }
        "pushFront" => {
            let value = args.pop().ok_or(VmError::Malformed("missing value argument"))?;
            data.borrow_mut().insert(0, value);
            Ok(None)
        }
        "popBack" => match data.borrow_mut().pop() {
            Some(v) => Ok(Some(v)),
            None => Err(throw_native("IndexOutOfBoundsException", "popBack on empty list")),
        },
        "popFront" => {
            let mut d = data.borrow_mut();
            if d.is_empty() {
                return Err(throw_native("IndexOutOfBoundsException", "popFront on empty list"));
            }
            Ok(Some(d.remove(0)))
        }
        "remove" => {
            let idx = expect_int(&mut args)?;
            let mut d = data.borrow_mut();
            if idx < 0 || idx as usize >= d.len() {
                return Err(throw_native("IndexOutOfBoundsException", format!("index {idx}, length {}", d.len())));
            }
            Ok(Some(d.remove(idx as usize)))
        }
        "contains" => {
            let value = args.pop().ok_or(VmError::Malformed("missing value argument"))?;
            Ok(Some(Value::Bool(data.borrow().iter().any(|v| values_equal(v, &value)))))
        }
        _ => Err(VmError::MethodNotFound(format!("system.List.{name}"))),
    }
}

fn map_storage(receiver: &Value) -> Result<(ArrayRc, ArrayRc), VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected Map receiver"));
    };
    let obj = obj.borrow();
    match (obj.fields.get("__keys__"), obj.fields.get("__values__")) {
        (Some(Value::Array(k)), Some(Value::Array(v))) => Ok((Rc::clone(k), Rc::clone(v))),
        _ => Err(VmError::Malformed("malformed Map object")),
    }
}

fn dispatch_map(name: &str, receiver: &Value, mut args: Vec<Value>) -> Result<Option<Value>, VmError> {
    let (keys, values) = map_storage(receiver)?;
    match name {
        "size" => Ok(Some(Value::Int(keys.borrow().len() as i64))),
        "get" => {
            let key = args.pop().ok_or(VmError::Malformed("missing key argument"))?;
            let idx = keys.borrow().iter().position(|k| values_equal(k, &key));
            Ok(Some(match idx {
                Some(i) => values.borrow()[i].clone(),
                None => Value::Null,
            }))
        }
        "set" => {
            let value = args.pop().ok_or(VmError::Malformed("missing value argument"))?;
            let key = args.pop().ok_or(VmError::Malformed("missing key argument"))?;
            let idx = keys.borrow().iter().position(|k| values_equal(k, &key));
            match idx {
                Some(i) => values.borrow_mut()[i] = value,
                None => {
                    keys.borrow_mut().push(key);
                    values.borrow_mut().push(value);
                }
            }
            Ok(None)
        }
        "remove" => {
            let key = args.pop().ok_or(VmError::Malformed("missing key argument"))?;
            let idx = keys.borrow().iter().position(|k| values_equal(k, &key));
            match idx {
                Some(i) => {
                    keys.borrow_mut().remove(i);
                    values.borrow_mut().remove(i);
                    Ok(Some(Value::Bool(true)))
                }
                None => Ok(Some(Value::Bool(false))),
            }
        }
        "has" => {
            let key = args.pop().ok_or(VmError::Malformed("missing key argument"))?;
            Ok(Some(Value::Bool(keys.borrow().iter().any(|k| values_equal(k, &key)))))
        }
        "keys" => Ok(Some(Value::Array(Rc::new(RefCell::new(keys.borrow().clone()))))),
        "values" => Ok(Some(Value::Array(Rc::new(RefCell::new(values.borrow().clone()))))),
        // stdlib.md § system.MapEntry — result objects with two public
        // fields, classed under the matching mangled `MapEntry`
        // instantiation (`"system.Map<string, int>"` ->
        // `"system.MapEntry<string, int>"`). Iteration order == `keys()`'s,
        // as the spec requires ("consistent").
        "entries" => {
            let Value::Object(obj) = receiver else {
                return Err(VmError::Malformed("expected Map receiver"));
            };
            let entry_class = format!("system.MapEntry<{}", &obj.borrow().class_name["system.Map<".len()..]);
            let entries: Vec<Value> = keys
                .borrow()
                .iter()
                .zip(values.borrow().iter())
                .map(|(k, v)| {
                    let mut fields = HashMap::new();
                    fields.insert("key".to_string(), k.clone());
                    fields.insert("value".to_string(), v.clone());
                    Value::Object(Rc::new(RefCell::new(Object { class_name: entry_class.clone(), fields })))
                })
                .collect();
            Ok(Some(Value::Array(Rc::new(RefCell::new(entries)))))
        }
        _ => Err(VmError::MethodNotFound(format!("system.Map.{name}"))),
    }
}
