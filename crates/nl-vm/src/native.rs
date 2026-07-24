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
//! `INVOKE_STATIC system.String.<name>`, see `nl_codegen::stdlib`),
//! `system.List<T>`/`system.Map<K,V>` (see the section below), and
//! `system.io.*` file I/O including `File.open`'s `FileMode` overload and
//! `File.glob` (backed by `crate::mini_regex`, since patterns are matched
//! as regex — see that module's doc comment), `system.io.Grep` (same
//! `mini_regex` backing, applied per-line instead of per-path), and
//! `system.text.Regex`/`system.text.Encoding` (also backed by `crate::mini_regex`, plus
//! `crate::text` for base64 and `RegexMatch` construction), and
//! `system.time.DateTime`/`system.time.TimeZone` (calendar math and IANA
//! zone lookups in `crate::mini_tz`), and `system.Env` (thin wrapper over
//! `std::env`).
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
//! Key/element equality for `contains`/map lookups goes through
//! `equatable_equals`: primitives and `string` by value, a reference type
//! implementing `ValueEquatable` by calling its `valueEquals` (virtually —
//! `resolve_virtual_by_name`, so an override further down the hierarchy
//! wins), everything else by reference identity (`interpreter::
//! values_equal`) — exactly stdlib.md's documented rule.
//!
//! `List` has no `forEach` of its own (stdlib.md doesn't define one, only
//! `Map.forEach` — see `dispatch_map`).
//!
//! ## Arrays (`T[]`)
//!
//! `length()` is the dedicated `ARRAY_LENGTH` opcode (performance-critical
//! per vm.md) and never reaches this module. The other six array methods
//! (`slice`/`map`/`filter`/`forEach`/`sort`/`find`) are `INVOKE_INSTANCE`
//! against a `Value::Array` receiver, intercepted in
//! `interpreter::exec_step` before the usual `Value::Object` receiver path
//! (arrays have no class of their own to dispatch by) and handled by
//! `dispatch_array`. The four callback-taking methods (plus `Map.forEach`)
//! all go through `invoke_closure`, which resolves and calls a closure
//! value's synthetic `invoke` method by name alone — the same mechanism
//! `system.thread.Thread`'s task already used (`invoke_task`), generalized
//! to take arguments.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::VmError;
use crate::interpreter::{
    call_instance, is_instance_of, resolve_virtual_by_name, values_equal,
};
use crate::program::Program;
use crate::value::{lock, Object, Value};

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
            | "system.io.Grep"
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

/// Dispatches one native call. `args` has already been popped off the
/// operand stack by the caller, in declaration order. Returns `Ok(None)`
/// for a `void` native (nothing to push back).
pub fn dispatch(
    program: &Arc<Program>,
    fqcn: &str,
    name: &str,
    mut args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
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
        ("system.In", "readLine") => match program.read_stdin_line() {
            Ok(Some(line)) => Ok(Some(Value::Str(Arc::new(line)))),
            Ok(None) => Ok(Some(Value::Null)), // EOF
            Err(e) => Err(VmError::Io(e)),
        },
        ("system.Int", "parse") => match expect_str(&mut args)?.trim().parse::<i64>() {
            Ok(v) => Ok(Some(Value::Int(v))),
            Err(_) => Err(throw_format_error("invalid int literal")),
        },
        ("system.Int", "tryParse") => match expect_str(&mut args)?.trim().parse::<i64>() {
            Ok(v) => Ok(Some(Value::Int(v))),
            Err(_) => Ok(Some(Value::Null)),
        },
        ("system.Int", "toString") => Ok(Some(Value::Str(Arc::new(
            expect_int(&mut args)?.to_string(),
        )))),
        ("system.Float", "parse") => match expect_str(&mut args)?.trim().parse::<f64>() {
            Ok(v) => Ok(Some(Value::Float(v))),
            Err(_) => Err(throw_format_error("invalid float literal")),
        },
        ("system.Float", "tryParse") => match expect_str(&mut args)?.trim().parse::<f64>() {
            Ok(v) => Ok(Some(Value::Float(v))),
            Err(_) => Ok(Some(Value::Null)),
        },
        ("system.Float", "toString") => Ok(Some(Value::Str(Arc::new(
            expect_float(&mut args)?.to_string(),
        )))),
        ("system.Bool", "parse") => match expect_str(&mut args)?.as_str() {
            "true" => Ok(Some(Value::Bool(true))),
            "false" => Ok(Some(Value::Bool(false))),
            _ => Err(throw_native(
                "IllegalArgumentException",
                "expected \"true\" or \"false\"",
            )),
        },
        ("system.Bool", "tryParse") => match expect_str(&mut args)?.as_str() {
            "true" => Ok(Some(Value::Bool(true))),
            "false" => Ok(Some(Value::Bool(false))),
            _ => Ok(Some(Value::Null)),
        },
        ("system.Bool", "toString") => Ok(Some(Value::Str(Arc::new(
            expect_bool(&mut args)?.to_string(),
        )))),
        // stdlib.md § system.String — `args[0]` is always the receiver
        // (whether the call came from `text.trim()` or the equivalent
        // static `system.String.trim(text)`, see nl_codegen::stdlib's doc
        // comment); indexed rather than popped since several of these take
        // more than one argument and popping would read them back to
        // front. Character positions are counted in `char`s, not bytes
        // (specs.md: "A character is represented as a string of length
        // 1").
        ("system.String", "length") => {
            Ok(Some(Value::Int(str_at(&args, 0)?.chars().count() as i64)))
        }
        ("system.String", "charAt") => {
            let chars: Vec<char> = str_at(&args, 0)?.chars().collect();
            let idx = int_at(&args, 1)?;
            if idx < 0 || idx as usize >= chars.len() {
                return Err(throw_native(
                    "IndexOutOfBoundsException",
                    format!("index {idx}, length {}", chars.len()),
                ));
            }
            Ok(Some(Value::Str(Arc::new(chars[idx as usize].to_string()))))
        }
        ("system.String", "substring") => {
            let chars: Vec<char> = str_at(&args, 0)?.chars().collect();
            let start = int_at(&args, 1)?;
            let end = if args.len() >= 3 {
                int_at(&args, 2)?
            } else {
                chars.len() as i64
            };
            if start < 0 || end < start || end as usize > chars.len() {
                return Err(throw_native(
                    "IndexOutOfBoundsException",
                    format!("start {start}, end {end}, length {}", chars.len()),
                ));
            }
            let sub: String = chars[start as usize..end as usize].iter().collect();
            Ok(Some(Value::Str(Arc::new(sub))))
        }
        ("system.String", "indexOf") => {
            let haystack = str_at(&args, 0)?;
            let needle = str_at(&args, 1)?;
            let from = if args.len() >= 3 {
                int_at(&args, 2)?.max(0) as usize
            } else {
                0
            };
            Ok(Some(Value::Int(
                char_index_of(&haystack, &needle, from).unwrap_or(-1),
            )))
        }
        ("system.String", "contains") => Ok(Some(Value::Bool(
            str_at(&args, 0)?.contains(&str_at(&args, 1)?),
        ))),
        ("system.String", "toUpperCase") => {
            Ok(Some(Value::Str(Arc::new(str_at(&args, 0)?.to_uppercase()))))
        }
        ("system.String", "toLowerCase") => {
            Ok(Some(Value::Str(Arc::new(str_at(&args, 0)?.to_lowercase()))))
        }
        ("system.String", "replace") => {
            let s = str_at(&args, 0)?;
            let from = str_at(&args, 1)?;
            let to = str_at(&args, 2)?;
            Ok(Some(Value::Str(Arc::new(s.replace(&from, &to)))))
        }
        ("system.String", "startsWith") => Ok(Some(Value::Bool(
            str_at(&args, 0)?.starts_with(&str_at(&args, 1)?),
        ))),
        ("system.String", "endsWith") => Ok(Some(Value::Bool(
            str_at(&args, 0)?.ends_with(&str_at(&args, 1)?),
        ))),
        ("system.String", "trim") => Ok(Some(Value::Str(Arc::new(
            str_at(&args, 0)?.trim().to_string(),
        )))),
        ("system.String", "split") => {
            let s = str_at(&args, 0)?;
            let delim = str_at(&args, 1)?;
            let parts: Vec<Value> = s
                .split(delim.as_str())
                .map(|p| Value::Str(Arc::new(p.to_string())))
                .collect();
            Ok(Some(Value::Array(Arc::new(Mutex::new(parts)))))
        }
        // stdlib.md § system.io.File — paths are used as-is, no
        // sanitization ("path validation is the caller's responsibility").
        ("system.io.File", "exists") => Ok(Some(Value::Bool(
            std::path::Path::new(&str_at(&args, 0)?).exists(),
        ))),
        ("system.io.File", "open") => {
            let path = str_at(&args, 0)?;
            // 1-argument open == `FileMode.ReadWrite` (stdlib.md): read and
            // write, file must already exist, positioned at the start. The
            // 2-argument form's mode int is the position of the variant
            // name in `nl_codegen::stdlib::enum_const_value`'s list (same
            // order as stdlib.md's `FileMode` table).
            let mode = if args.len() > 1 { int_at(&args, 1)? } else { 3 };
            let mut opts = std::fs::OpenOptions::new();
            match mode {
                0 => opts.read(true),                                         // Read
                1 => opts.write(true).create(true).truncate(true),            // Write
                2 => opts.write(true).create(true).append(true),              // Append
                3 => opts.read(true).write(true),                             // ReadWrite
                4 => opts.read(true).write(true).create(true).truncate(true), // ReadWriteTruncate
                5 => opts.read(true).write(true).create(true).append(true),   // ReadWriteAppend
                _ => return Err(VmError::Malformed("invalid FileMode value")),
            };
            let file = opts.open(&path).map_err(|e| throw_io_error(&path, e))?;
            let id = program.register_file(file);
            let mut fields = HashMap::new();
            fields.insert("__fd__".to_string(), Value::Int(id));
            Ok(Some(Value::Object(Arc::new(Mutex::new(Object::native(
                "system.io.FileHandle",
                fields,
            ))))))
        }
        ("system.io.File", "readAllText") => {
            let path = str_at(&args, 0)?;
            let text = std::fs::read_to_string(&path).map_err(|e| throw_io_error(&path, e))?;
            Ok(Some(Value::Str(Arc::new(text))))
        }
        ("system.io.File", "writeAllText") => {
            let path = str_at(&args, 0)?;
            let content = str_at(&args, 1)?;
            std::fs::write(&path, content).map_err(|e| throw_io_error(&path, e))?;
            Ok(None)
        }
        ("system.io.File", "glob") => {
            let base = str_at(&args, 0)?;
            let pattern = str_at(&args, 1)?;
            let regex = crate::mini_regex::Regex::compile(&pattern).map_err(|e| {
                throw_native(
                    "IOException",
                    format!("invalid glob pattern '{pattern}': {e}"),
                )
            })?;
            let base_path = std::path::Path::new(&base);
            let mut matches = Vec::new();
            collect_glob_matches(base_path, base_path, &regex, &mut matches)
                .map_err(|e| throw_io_error(&base, e))?;
            matches.sort();
            let values = matches
                .into_iter()
                .map(|p| Value::Str(Arc::new(p)))
                .collect();
            Ok(Some(Value::Array(Arc::new(Mutex::new(values)))))
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
            let values = names.into_iter().map(|n| Value::Str(Arc::new(n))).collect();
            Ok(Some(Value::Array(Arc::new(Mutex::new(values)))))
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
        ("system.io.Directory", "exists") => Ok(Some(Value::Bool(
            std::path::Path::new(&str_at(&args, 0)?).is_dir(),
        ))),
        ("system.io.Path", "join") => {
            let Some(Value::Array(segments)) = args.first() else {
                return Err(VmError::Malformed(
                    "expected string[] argument to native call",
                ));
            };
            let mut joined = std::path::PathBuf::new();
            for seg in lock(&segments).iter() {
                let Value::Str(s) = seg else {
                    return Err(VmError::Malformed(
                        "expected string[] argument to native call",
                    ));
                };
                joined.push(s.as_str());
            }
            Ok(Some(Value::Str(Arc::new(
                joined.to_string_lossy().into_owned(),
            ))))
        }
        ("system.io.Path", "dirname") => {
            let path = str_at(&args, 0)?;
            let dir = std::path::Path::new(&path)
                .parent()
                .map(|p| p.to_string_lossy().into_owned());
            Ok(Some(Value::Str(Arc::new(dir.unwrap_or_default()))))
        }
        ("system.io.Path", "basename") => {
            let path = str_at(&args, 0)?;
            let base = std::path::Path::new(&path)
                .file_name()
                .map(|p| p.to_string_lossy().into_owned());
            Ok(Some(Value::Str(Arc::new(base.unwrap_or_default()))))
        }
        ("system.io.Path", "extension") => {
            let path = str_at(&args, 0)?;
            // stdlib.md: "Returns the file extension (e.g. `.nl`)" — with
            // the leading dot, or null when there is none.
            match std::path::Path::new(&path).extension() {
                Some(ext) => Ok(Some(Value::Str(Arc::new(format!(
                    ".{}",
                    ext.to_string_lossy()
                ))))),
                None => Ok(Some(Value::Null)),
            }
        }
        ("system.io.Path", "normalize") => Ok(Some(Value::Str(Arc::new(normalize_path(&str_at(
            &args, 0,
        )?))))),
        // stdlib.md § system.io.Grep — line-oriented regex search, backed by
        // `crate::mini_regex` like `system.text.Regex`/`File.glob` above.
        // The two `search` overloads are told apart by `args.len()` (2 =
        // single file, 3 = dirPath + recursive flag), same trick as
        // `File.open`'s `FileMode` overload. Uses `Regex::find` (partial
        // /anywhere match — stdlib.md's own wording for `Regex.match` is
        // "like grep"), not `is_match` (reserved for `File.glob`'s
        // whole-path semantics).
        ("system.io.Grep", "search") => {
            let pattern = str_at(&args, 0)?;
            let path = str_at(&args, 1)?;
            let regex = compile_regex(&pattern)?;
            let mut matches = Vec::new();
            if args.len() > 2 {
                let recursive = bool_at(&args, 2)?;
                grep_path(std::path::Path::new(&path), &regex, recursive, &mut matches)
                    .map_err(|e| throw_io_error(&path, e))?;
            } else {
                grep_file(std::path::Path::new(&path), &regex, &mut matches)
                    .map_err(|e| throw_io_error(&path, e))?;
            }
            Ok(Some(Value::Array(Arc::new(Mutex::new(matches)))))
        }
        // stdlib.md § system.SecureRandom — CSPRNG backed by `/dev/urandom`,
        // same source `system.Uuid.random` uses. Not seedable.
        ("system.SecureRandom", "nextBytes") => {
            let Some(Value::Array(buffer)) = args.first().cloned() else {
                return Err(VmError::Malformed(
                    "expected byte[] argument to native call",
                ));
            };
            let len = lock(&buffer).len();
            let random = secure_random_bytes(len)?;
            let mut buf = lock(&buffer);
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
        ("system.Uuid", "random") => Ok(Some(Value::Str(Arc::new(uuid_v4()?)))),
        // stdlib.md § system.Env — thin wrapper over the process
        // environment. `set`/`remove` call `std::env::set_var`/`remove_var`,
        // which Rust itself marks `unsafe` (since 1.82) because mutating the
        // environment races with any other thread reading it concurrently —
        // exactly the UB stdlib.md's own thread-safety note warns about
        // ("Synchronize with `system.thread.Mutex`, or set all variables
        // from the main thread before spawning threads"). The VM has no way
        // to enforce that from here; it's on the NL program to follow it.
        ("system.Env", "get") => {
            let name = str_at(&args, 0)?;
            match std::env::var(&name) {
                Ok(value) => Ok(Some(Value::Str(Arc::new(value)))),
                Err(_) => Ok(Some(Value::Null)),
            }
        }
        ("system.Env", "set") => {
            let name = str_at(&args, 0)?;
            let value = str_at(&args, 1)?;
            // SAFETY: see the module-level comment above this match arm —
            // synchronization across threads is the calling NL program's
            // responsibility, per stdlib.md.
            unsafe { std::env::set_var(&name, &value) };
            Ok(None)
        }
        ("system.Env", "remove") => {
            let name = str_at(&args, 0)?;
            // SAFETY: see the module-level comment above this match arm.
            unsafe { std::env::remove_var(&name) };
            Ok(None)
        }
        ("system.Env", "list") => {
            let mut names: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
            // Iteration order isn't guaranteed stable across platforms;
            // sorted for deterministic `expected_stdout` in tests (same
            // rationale as `system.io.Directory.list`).
            names.sort();
            let values = names.into_iter().map(|n| Value::Str(Arc::new(n))).collect();
            Ok(Some(Value::Array(Arc::new(Mutex::new(values)))))
        }
        // stdlib.md § system.net.TcpStream — the one *static* TcpStream
        // method (the rest is instance dispatch, see `dispatch_tcp_stream`);
        // same object-building shape as `system.io.File.open`.
        ("system.net.TcpStream", "connect") => {
            let host = str_at(&args, 0)?;
            let port = int_at(&args, 1)?;
            let stream = std::net::TcpStream::connect((host.as_str(), port as u16))
                .map_err(|e| throw_native("IOException", format!("connect {host}:{port}: {e}")))?;
            let id = program.register_tcp_stream(stream);
            let mut fields = HashMap::new();
            fields.insert("__fd__".to_string(), Value::Int(id));
            Ok(Some(Value::Object(Arc::new(Mutex::new(Object::native(
                "system.net.TcpStream",
                fields,
            ))))))
        }
        ("system.net.Http", "get") => {
            let url = str_at(&args, 0)?;
            crate::net_http::http_request(&url, "GET", None).map(Some)
        }
        ("system.net.Http", "post") => {
            let url = str_at(&args, 0)?;
            let body = str_at(&args, 1)?;
            crate::net_http::http_request(&url, "POST", Some(&body)).map(Some)
        }
        ("system.thread.Thread", "sleep") => {
            let millis = expect_int(&mut args)?.max(0) as u64;
            std::thread::sleep(std::time::Duration::from_millis(millis));
            Ok(None)
        }
        // stdlib.md § system.ps.Process — `list`/`list(pid)` read `/proc`
        // directly (Linux-only, same portability stance already taken for
        // `system.SecureRandom`/`Uuid`'s `/dev/urandom`: this project's
        // dev/CI environment is Linux and no external crate is pulled in
        // just for process enumeration). `run` shells out and captures
        // output; `exit` doesn't produce a value at all — see `VmError::Exit`.
        ("system.ps.Process", "list") => {
            let usernames = read_passwd_usernames();
            let infos: Vec<Value> = if args.is_empty() {
                let mut pids: Vec<u32> = std::fs::read_dir("/proc")
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
                    .collect();
                pids.sort_unstable();
                pids.into_iter()
                    .filter_map(|pid| read_process_info(pid, &usernames))
                    .collect()
            } else {
                let pid = int_at(&args, 0)?;
                read_process_info(pid as u32, &usernames)
                    .into_iter()
                    .collect()
            };
            Ok(Some(Value::Array(Arc::new(Mutex::new(infos)))))
        }
        ("system.ps.Process", "run") => {
            let output = match args.first() {
                Some(Value::Str(cmd)) => std::process::Command::new("sh")
                    .arg("-c")
                    .arg(cmd.as_str())
                    .output(),
                Some(Value::Array(items)) => {
                    let parts: Vec<String> = lock(items)
                        .iter()
                        .map(|v| match v {
                            Value::Str(s) => Ok((**s).clone()),
                            _ => Err(VmError::Malformed(
                                "expected string[] argument to native call",
                            )),
                        })
                        .collect::<Result<_, _>>()?;
                    let Some((program_name, rest)) = parts.split_first() else {
                        return Err(throw_native("IOException", "empty command"));
                    };
                    std::process::Command::new(program_name).args(rest).output()
                }
                _ => {
                    return Err(VmError::Malformed(
                        "expected string or string[] argument to native call",
                    ))
                }
            };
            let output = output.map_err(|e| throw_native("IOException", format!("{e}")))?;
            let mut fields = HashMap::new();
            fields.insert(
                "exitCode".to_string(),
                Value::Int(output.status.code().unwrap_or(-1) as i64),
            );
            fields.insert(
                "stdout".to_string(),
                Value::Str(Arc::new(
                    String::from_utf8_lossy(&output.stdout).into_owned(),
                )),
            );
            fields.insert(
                "stderr".to_string(),
                Value::Str(Arc::new(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                )),
            );
            Ok(Some(Value::Object(Arc::new(Mutex::new(Object::native(
                "system.ps.ProcessResult",
                fields,
            ))))))
        }
        ("system.ps.Process", "pid") => Ok(Some(Value::Int(std::process::id() as i64))),
        // Never actually returns to the caller — see `VmError::Exit`'s doc
        // comment for why this isn't a literal `std::process::exit`.
        ("system.ps.Process", "exit") => Err(VmError::Exit(expect_int(&mut args)? as i32)),
        ("system.ps.Process", "getCwd") => {
            let cwd = std::env::current_dir().map_err(VmError::Io)?;
            Ok(Some(Value::Str(Arc::new(
                cwd.to_string_lossy().into_owned(),
            ))))
        }
        ("system.ps.Process", "setCwd") => {
            let path = str_at(&args, 0)?;
            std::env::set_current_dir(&path).map_err(|e| throw_io_error(&path, e))?;
            Ok(None)
        }
        // stdlib.md § system.text.Regex — `match` is a partial (anywhere)
        // search like grep/`preg_match` (`mini_regex::Regex::find`, not
        // `is_match`, which is reserved for `File.glob`'s whole-path
        // semantics); an invalid pattern has no documented exception, so
        // `IllegalArgumentException` (unchecked) is used, matching this
        // codebase's other "bad input, no `throws` in stdlib.md" cases
        // (`Random.nextInt(bound<=0)`, `Semaphore(initialCount<0)`).
        ("system.text.Regex", "match") => {
            let pattern = str_at(&args, 0)?;
            let input = str_at(&args, 1)?;
            let regex = compile_regex(&pattern)?;
            Ok(Some(Value::Bool(regex.find(&input).is_some())))
        }
        ("system.text.Regex", "matchFirst") => {
            let pattern = str_at(&args, 0)?;
            let input = str_at(&args, 1)?;
            let regex = compile_regex(&pattern)?;
            let chars: Vec<char> = input.chars().collect();
            match regex.find(&input) {
                Some(m) => Ok(Some(crate::text::build_regex_match(&m, &chars))),
                None => Ok(Some(Value::Null)),
            }
        }
        ("system.text.Regex", "replace") => {
            let pattern = str_at(&args, 0)?;
            let input = str_at(&args, 1)?;
            let replacement = str_at(&args, 2)?;
            let regex = compile_regex(&pattern)?;
            let chars: Vec<char> = input.chars().collect();
            let mut out = String::new();
            let mut last = 0;
            for m in regex.find_all(&input) {
                out.extend(&chars[last..m.start]);
                out.push_str(&replacement);
                last = m.end;
            }
            out.extend(&chars[last..]);
            Ok(Some(Value::Str(Arc::new(out))))
        }
        ("system.text.Regex", "split") => {
            let pattern = str_at(&args, 0)?;
            let input = str_at(&args, 1)?;
            let regex = compile_regex(&pattern)?;
            let chars: Vec<char> = input.chars().collect();
            let mut parts = Vec::new();
            let mut last = 0;
            for m in regex.find_all(&input) {
                parts.push(Value::Str(Arc::new(chars[last..m.start].iter().collect())));
                last = m.end;
            }
            parts.push(Value::Str(Arc::new(chars[last..].iter().collect())));
            Ok(Some(Value::Array(Arc::new(Mutex::new(parts)))))
        }
        ("system.text.Regex", "escape") => Ok(Some(Value::Str(Arc::new(
            crate::mini_regex::escape(&str_at(&args, 0)?),
        )))),
        // stdlib.md § system.text.Encoding — byte<->string/base64
        // conversion, no external crate (same stance as `crate::mini_regex`
        // /`system.SecureRandom`'s `/dev/urandom`). `decodeUtf8` is lossy
        // (replaces invalid sequences) rather than throwing: stdlib.md
        // documents no exception for it, unlike `base64Decode`.
        ("system.text.Encoding", "encodeUtf8") => {
            Ok(Some(array_from_bytes(str_at(&args, 0)?.into_bytes())))
        }
        ("system.text.Encoding", "decodeUtf8") => {
            let bytes = bytes_from_array(args.first())?;
            Ok(Some(Value::Str(Arc::new(
                String::from_utf8_lossy(&bytes).into_owned(),
            ))))
        }
        ("system.text.Encoding", "base64Encode") => {
            let bytes = bytes_from_array(args.first())?;
            Ok(Some(Value::Str(Arc::new(crate::text::base64_encode(
                &bytes,
            )))))
        }
        ("system.text.Encoding", "base64Decode") => {
            let s = str_at(&args, 0)?;
            let bytes =
                crate::text::base64_decode(&s).map_err(|e| throw_native("FormatException", e))?;
            Ok(Some(array_from_bytes(bytes)))
        }
        // stdlib.md § system.time.DateTime — `now()`/`now(zone)` share one
        // match arm (arity decides which, same trick as
        // `SecureRandom.nextInt`); `parse` throws `FormatException` per the
        // declared signature (checked, see `nl_sema::stdlib::throws`).
        ("system.time.DateTime", "now") => {
            let zone = match args.first() {
                Some(v) => timezone_id(v)?,
                None => crate::mini_tz::default_zone_id(),
            };
            Ok(Some(new_datetime_object(
                crate::mini_tz::now_epoch_secs(),
                zone,
            )))
        }
        ("system.time.DateTime", "parse") => {
            let s = str_at(&args, 0)?;
            let (epoch, zone) = crate::mini_tz::parse_iso8601(&s)
                .map_err(|e| throw_native("FormatException", e))?;
            Ok(Some(new_datetime_object(epoch, zone)))
        }
        // stdlib.md § system.time.TimeZone — `get` validates the id by
        // actually resolving an offset for it (unknown id or unparseable
        // `/usr/share/zoneinfo` entry -> `IllegalArgumentException`,
        // unchecked, same "bad input, no `throws` documented" convention as
        // `Random.nextInt(bound<=0)`).
        ("system.time.TimeZone", "getDefault") => {
            Ok(Some(new_timezone_object(crate::mini_tz::default_zone_id())))
        }
        ("system.time.TimeZone", "get") => {
            let id = str_at(&args, 0)?;
            crate::mini_tz::zone_offset_seconds(&id, crate::mini_tz::now_epoch_secs())
                .map_err(|e| throw_native("IllegalArgumentException", e))?;
            Ok(Some(new_timezone_object(id)))
        }
        _ => Err(VmError::MethodNotFound(format!("{fqcn}.{name}"))),
    }
}

/// `uid -> username` from `/etc/passwd` (`name:passwd:uid:gid:gecos:home:shell`),
/// used by `read_process_info` to resolve `ProcessInfo.user`. Read fresh on
/// every `Process.list()` call rather than cached — process listing is
/// already an inherently point-in-time snapshot, and re-reading a small
/// text file per call keeps this stateless like every other native here.
fn read_passwd_usernames() -> HashMap<u32, String> {
    let mut map = HashMap::new();
    if let Ok(content) = std::fs::read_to_string("/etc/passwd") {
        for line in content.lines() {
            let fields: Vec<&str> = line.split(':').collect();
            if let [name, _passwd, uid, ..] = fields.as_slice() {
                if let Ok(uid) = uid.parse::<u32>() {
                    map.entry(uid).or_insert_with(|| name.to_string());
                }
            }
        }
    }
    map
}

/// One `system.ps.ProcessInfo` for `pid`, or `None` if `/proc/<pid>` doesn't
/// exist (already gone, or the caller has no permission to see it — treated
/// the same as "not found", matching `Process.list(pid)`'s documented
/// "empty array if not found"). `command`/`args` come from `/proc/<pid>/cmdline`
/// (NUL-separated: first entry is the command, the rest are arguments); a
/// kernel thread or zombie has an empty `cmdline`, so `command` falls back to
/// `/proc/<pid>/comm` (always present) with no args. `user` comes from the
/// first `Uid:` field of `/proc/<pid>/status`, resolved through
/// `read_passwd_usernames` — `None` (not just an unknown uid) if `status`
/// itself can't be read, per stdlib.md's "null if not available".
fn read_process_info(pid: u32, usernames: &HashMap<u32, String>) -> Option<Value> {
    let proc_dir = format!("/proc/{pid}");
    if !std::path::Path::new(&proc_dir).is_dir() {
        return None;
    }
    let cmdline_raw = std::fs::read(format!("{proc_dir}/cmdline")).unwrap_or_default();
    let mut parts: Vec<String> = cmdline_raw
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let command = if parts.is_empty() {
        std::fs::read_to_string(format!("{proc_dir}/comm"))
            .map(|s| s.trim_end().to_string())
            .unwrap_or_default()
    } else {
        parts.remove(0)
    };
    let user = std::fs::read_to_string(format!("{proc_dir}/status"))
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Uid:")?
                        .split_whitespace()
                        .next()?
                        .parse::<u32>()
                        .ok()
                })
                .and_then(|uid| usernames.get(&uid).cloned())
        });

    let mut fields = HashMap::new();
    fields.insert("pid".to_string(), Value::Int(pid as i64));
    fields.insert("command".to_string(), Value::Str(Arc::new(command)));
    fields.insert(
        "args".to_string(),
        Value::Array(Arc::new(Mutex::new(
            parts.into_iter().map(|a| Value::Str(Arc::new(a))).collect(),
        ))),
    );
    fields.insert(
        "user".to_string(),
        user.map_or(Value::Null, |u| Value::Str(Arc::new(u))),
    );
    Some(Value::Object(Arc::new(Mutex::new(Object::native(
        "system.ps.ProcessInfo",
        fields,
    )))))
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
        return Err(throw_native(
            "IllegalArgumentException",
            "bound must be positive",
        ));
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

/// Recursively walks `dir` (starting at `base`, the `glob` call's
/// `basePath`), testing each regular file's path *relative to `base`*
/// (forward-slash separated, even on Windows, so patterns are portable)
/// against `regex`, and collecting the *full* path of every match — per
/// stdlib.md: "an array of full paths under basePath whose relative path
/// matches pattern". Directories themselves are never matched, only
/// recursed into; symlinks are followed (`Path::is_dir` does), consistent
/// with stdlib.md's path-traversal warning for the rest of `system.io.File`.
fn collect_glob_matches(
    base: &std::path::Path,
    dir: &std::path::Path,
    regex: &crate::mini_regex::Regex,
    out: &mut Vec<String>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_glob_matches(base, &path, regex, out)?;
        } else {
            let rel = path.strip_prefix(base).unwrap_or(&path);
            let rel_str = rel
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            if regex.is_match(&rel_str) {
                out.push(path.to_string_lossy().into_owned());
            }
        }
    }
    Ok(())
}

/// `system.io.Grep.search` on a single file — appends one `GrepMatch` per
/// line that `regex` matches anywhere in (partial match, see the dispatch
/// arm's doc comment).
fn grep_file(
    path: &std::path::Path,
    regex: &crate::mini_regex::Regex,
    out: &mut Vec<Value>,
) -> std::io::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let path_str = path.to_string_lossy().into_owned();
    for (i, line) in content.lines().enumerate() {
        if regex.find(line).is_some() {
            out.push(build_grep_match(&path_str, (i + 1) as i64, line));
        }
    }
    Ok(())
}

/// `system.io.Grep.search(pattern, dirPath, recursive)` — stdlib.md: "If
/// `recursive` is `true`, searches all files under `dirPath`; otherwise only
/// the file or directory at `dirPath`" (non-recursive over a directory means
/// its immediate file children only, not their subdirectories). Directory
/// entries are visited in sorted order for the same stable-output reason as
/// `collect_glob_matches`/`Directory.list` above.
fn grep_path(
    path: &std::path::Path,
    regex: &crate::mini_regex::Regex,
    recursive: bool,
    out: &mut Vec<Value>,
) -> std::io::Result<()> {
    if !path.is_dir() {
        return grep_file(path, regex, out);
    }
    let mut entries: Vec<_> = std::fs::read_dir(path)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let child = entry.path();
        if child.is_dir() {
            if recursive {
                grep_path(&child, regex, recursive, out)?;
            }
        } else {
            grep_file(&child, regex, out)?;
        }
    }
    Ok(())
}

/// Builds a `system.io.GrepMatch` object (stdlib.md § Result types: `path:
/// string`, `lineNumber: int`, `line: string`) — same shape as
/// `crate::text::build_regex_match`, but simple enough (no capture groups)
/// not to warrant its own module.
fn build_grep_match(path: &str, line_number: i64, line: &str) -> Value {
    let mut fields = HashMap::new();
    fields.insert("path".to_string(), Value::Str(Arc::new(path.to_string())));
    fields.insert("lineNumber".to_string(), Value::Int(line_number));
    fields.insert("line".to_string(), Value::Str(Arc::new(line.to_string())));
    Value::Object(Arc::new(Mutex::new(Object::native(
        "system.io.GrepMatch",
        fields,
    ))))
}

/// Maps a host I/O error to the spec's exception types — stdlib.md:
/// `FileNotFoundException` when the path does not exist, `IOException` for
/// every other failure.
fn throw_io_error(path: &str, err: std::io::Error) -> VmError {
    match err.kind() {
        std::io::ErrorKind::NotFound => {
            throw_native("FileNotFoundException", format!("{path}: {err}"))
        }
        _ => throw_native("IOException", format!("{path}: {err}")),
    }
}

fn str_at(args: &[Value], i: usize) -> Result<String, VmError> {
    match args.get(i) {
        Some(Value::Str(s)) => Ok((**s).clone()),
        _ => Err(VmError::Malformed(
            "expected string argument to native call",
        )),
    }
}

fn int_at(args: &[Value], i: usize) -> Result<i64, VmError> {
    args.get(i)
        .and_then(|v| v.as_int())
        .ok_or(VmError::Malformed("expected int argument to native call"))
}

fn bool_at(args: &[Value], i: usize) -> Result<bool, VmError> {
    args.get(i)
        .and_then(|v| v.as_bool())
        .ok_or(VmError::Malformed("expected bool argument to native call"))
}

/// Char-index (not byte-index) of the first occurrence of `needle` in
/// `haystack` at or after char position `from`, or `None`. An empty
/// `needle` matches at `from` itself, mirroring `str::find`'s behavior.
fn char_index_of(haystack: &str, needle: &str, from: usize) -> Option<i64> {
    let hay: Vec<char> = haystack.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() {
        return if from <= hay.len() {
            Some(from as i64)
        } else {
            None
        };
    }
    if from > hay.len() || needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len())
        .find(|&start| hay[start..start + needle.len()] == needle[..])
        .map(|s| s as i64)
}

fn expect_str(args: &mut Vec<Value>) -> Result<String, VmError> {
    match args.pop() {
        Some(Value::Str(s)) => Ok((*s).clone()),
        _ => Err(VmError::Malformed(
            "expected string argument to native call",
        )),
    }
}

fn expect_int(args: &mut Vec<Value>) -> Result<i64, VmError> {
    args.pop()
        .and_then(|v| v.as_int())
        .ok_or(VmError::Malformed("expected int argument to native call"))
}

fn expect_float(args: &mut Vec<Value>) -> Result<f64, VmError> {
    args.pop()
        .and_then(|v| v.as_float())
        .ok_or(VmError::Malformed("expected float argument to native call"))
}

fn expect_bool(args: &mut Vec<Value>) -> Result<bool, VmError> {
    args.pop()
        .and_then(|v| v.as_bool())
        .ok_or(VmError::Malformed("expected bool argument to native call"))
}

fn throw_format_error(message: impl Into<String>) -> VmError {
    throw_native("NumberFormatException", message)
}

/// Shared by every `system.text.Regex` dispatch arm — see that match arm's
/// doc comment for why an invalid pattern is `IllegalArgumentException`.
fn compile_regex(pattern: &str) -> Result<crate::mini_regex::Regex, VmError> {
    crate::mini_regex::Regex::compile(pattern).map_err(|e| {
        throw_native(
            "IllegalArgumentException",
            format!("invalid regex pattern '{pattern}': {e}"),
        )
    })
}

/// Reads a `byte[]` argument's elements into an owned `Vec<u8>` — used by
/// `system.text.Encoding`'s decode paths, which (unlike `SecureRandom.nextBytes`
/// or `FileHandle.read`/`write`) don't already have a `byte[]` buffer to fill
/// in place.
fn bytes_from_array(arg: Option<&Value>) -> Result<Vec<u8>, VmError> {
    let Some(Value::Array(arr)) = arg else {
        return Err(VmError::Malformed(
            "expected byte[] argument to native call",
        ));
    };
    lock(arr)
        .iter()
        .map(|item| match item {
            Value::Byte(b) => Ok(*b),
            _ => Err(VmError::Malformed(
                "expected byte[] argument to native call",
            )),
        })
        .collect()
}

fn array_from_bytes(bytes: Vec<u8>) -> Value {
    Value::Array(Arc::new(Mutex::new(
        bytes.into_iter().map(Value::Byte).collect(),
    )))
}

pub(crate) fn throw_native(class_name: &str, message: impl Into<String>) -> VmError {
    let mut fields = HashMap::new();
    fields.insert("message".to_string(), Value::Str(Arc::new(message.into())));
    VmError::Thrown(Value::Object(Arc::new(Mutex::new(Object::native(
        class_name, fields,
    )))))
}

/// `system.io.FileHandle` and `system.Random` — like the native generic
/// collections below, real heap objects dispatched through
/// `INVOKE_INSTANCE` on their runtime class. `FileHandle` is stateful
/// *outside* the object (an `"__fd__"` index into `Program::file_handles`,
/// which is why `dispatch_native_instance` takes `program`); `Random`
/// instead keeps its PRNG state directly on the object (`"__state__"`, see
/// `is_random_class`/`dispatch_random` below) and ignores `program`
/// entirely. `system.time.DateTime`/`system.time.TimeZone` are the same
/// state-on-the-object shape as `Random` (`"__epoch__"`/`"__zone__"`,
/// `"__id__"` — see `dispatch_datetime`/`dispatch_timezone` below); neither
/// is ever constructed with `new` (only by the static factories `now`/
/// `parse`/`getDefault`/`get`, wired in `dispatch` above, the same way
/// `File.open` builds a `FileHandle`), so unlike `Random` they never appear
/// at `NEW`/`INVOKE_SPECIAL <construct>`.
pub fn is_native_instance_class(fqcn: &str) -> bool {
    matches!(
        fqcn,
        "system.io.FileHandle"
            | "system.Random"
            | "system.net.TcpListener"
            | "system.net.TcpStream"
            | "system.net.UdpSocket"
            | "system.thread.Thread"
            | "system.thread.Mutex"
            | "system.thread.Semaphore"
            | "system.time.DateTime"
            | "system.time.TimeZone"
    )
}

pub fn dispatch_native_instance(
    program: &Arc<Program>,
    name: &str,
    receiver: &Value,
    args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    use std::io::{Read, Write};

    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected native instance receiver"));
    };
    // A `.clone()`'d owned `String` rather than matching directly on
    // `lock(&obj).class_name.as_str()` — a match scrutinee's temporary
    // guard is kept alive for the whole match, including the arm bodies,
    // and `dispatch_random`/`dispatch_tcp_*`/`dispatch_thread` all
    // re-lock the same `Mutex` (e.g. `dispatch_random`'s `lock(&obj)`),
    // which would deadlock otherwise.
    let class_name = lock(&obj).class_name.clone();
    match class_name.as_str() {
        "system.Random" => return dispatch_random(name, receiver, args),
        "system.net.TcpListener" => return dispatch_tcp_listener(program, name, receiver, args),
        "system.net.TcpStream" => return dispatch_tcp_stream(program, name, receiver, args),
        "system.net.UdpSocket" => return dispatch_udp_socket(program, name, receiver, args),
        "system.thread.Thread" => return dispatch_thread(program, name, receiver, args),
        "system.thread.Mutex" => return dispatch_mutex(program, name, receiver, args),
        "system.thread.Semaphore" => return dispatch_semaphore(program, name, receiver, args),
        "system.time.DateTime" => return dispatch_datetime(name, receiver, args),
        "system.time.TimeZone" => return dispatch_timezone(name, receiver, args),
        _ => {}
    }
    let id = match lock(&obj).fields.get("__fd__") {
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
                return Err(VmError::Malformed(
                    "expected byte[] argument to native call",
                ));
            };
            let offset = int_at(&args, 1)?;
            let length = int_at(&args, 2)?;
            let buf_len = lock(&buffer).len() as i64;
            // stdlib.md § system.io.FileHandle, Bounds checking: checked
            // *before any I/O*, immune to `offset + length` overflow
            // (checked_add instead of wrapping `+`).
            if offset < 0
                || length < 0
                || offset.checked_add(length).is_none_or(|end| end > buf_len)
            {
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
                let mut buf = lock(&buffer);
                for (i, byte) in tmp[..n].iter().enumerate() {
                    buf[offset as usize + i] = Value::Byte(*byte);
                }
                Ok(Some(Value::Int(n as i64)))
            } else {
                let data: Vec<u8> = lock(&buffer)[offset as usize..(offset + length) as usize]
                    .iter()
                    .map(|v| match v {
                        Value::Byte(b) => Ok(*b),
                        // `int` stored through a `byte[]` element keeps the
                        // low-order bits, same as the `(byte)` cast rule.
                        Value::Int(i) => Ok(*i as u8),
                        _ => Err(VmError::Malformed(
                            "expected byte[] argument to native call",
                        )),
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
                            return Ok(if bytes.is_empty() {
                                None
                            } else {
                                Some(lossy_line(bytes))
                            });
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
                Some(l) => Value::Str(Arc::new(l)),
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
        _ => Err(VmError::MethodNotFound(format!(
            "system.io.FileHandle.{name}"
        ))),
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
    Value::Object(Arc::new(Mutex::new(Object::native(
        "system.Random",
        fields,
    ))))
}

pub fn construct_random(receiver: &Value, mut args: Vec<Value>) -> Result<(), VmError> {
    let seed = match args.pop() {
        Some(v) => v.as_int().ok_or(VmError::Malformed(
            "expected int seed argument to native call",
        ))? as u64,
        None => default_random_seed(),
    };
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected Random receiver"));
    };
    lock(&obj)
        .fields
        .insert("__state__".to_string(), Value::Int(seed as i64));
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

fn dispatch_random(
    name: &str,
    receiver: &Value,
    mut args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected Random receiver"));
    };
    let mut state = match lock(&obj).fields.get("__state__") {
        Some(Value::Int(s)) => *s as u64,
        _ => return Err(VmError::Malformed("malformed Random object")),
    };
    let result = match name {
        "nextInt" if args.is_empty() => Value::Int(splitmix64_next(&mut state) as i64),
        "nextInt" => {
            let bound = expect_int(&mut args)?;
            if bound <= 0 {
                return Err(throw_native(
                    "IllegalArgumentException",
                    "bound must be positive",
                ));
            }
            Value::Int((splitmix64_next(&mut state) % bound as u64) as i64)
        }
        "nextFloat" => {
            let raw = splitmix64_next(&mut state) >> 11; // top 53 bits
            Value::Float(raw as f64 * (1.0 / (1u64 << 53) as f64))
        }
        _ => return Err(VmError::MethodNotFound(format!("system.Random.{name}"))),
    };
    lock(&obj)
        .fields
        .insert("__state__".to_string(), Value::Int(state as i64));
    Ok(Some(result))
}

/// stdlib.md § system.time.DateTime/TimeZone — a `DateTime` is a UTC instant
/// (`"__epoch__"`, whole seconds since the Unix epoch — no sub-second
/// resolution) plus a zone id (`"__zone__"`, either an IANA name like
/// `"Europe/Paris"` or the `"+HH:MM"`/`"-HH:MM"`/`"UTC"` forms
/// `crate::mini_tz` also accepts); a `TimeZone` is just that same id
/// (`"__id__"`) wrapped as its own object so `getTimeZone()`/`TimeZone.get`
/// have something to return. All the actual calendar math and zone-offset
/// lookups live in `crate::mini_tz`; this module only shuttles object fields
/// in and out of it.
fn new_datetime_object(epoch: i64, zone: String) -> Value {
    let mut fields = HashMap::new();
    fields.insert("__epoch__".to_string(), Value::Int(epoch));
    fields.insert("__zone__".to_string(), Value::Str(Arc::new(zone)));
    Value::Object(Arc::new(Mutex::new(Object::native(
        "system.time.DateTime",
        fields,
    ))))
}

fn new_timezone_object(id: String) -> Value {
    let mut fields = HashMap::new();
    fields.insert("__id__".to_string(), Value::Str(Arc::new(id)));
    Value::Object(Arc::new(Mutex::new(Object::native(
        "system.time.TimeZone",
        fields,
    ))))
}

/// Extracts `__id__` from a `TimeZone` argument (`DateTime.now(zone)`).
fn timezone_id(v: &Value) -> Result<String, VmError> {
    let Value::Object(obj) = v else {
        return Err(VmError::Malformed(
            "expected TimeZone argument to native call",
        ));
    };
    match lock(obj).fields.get("__id__") {
        Some(Value::Str(s)) => Ok((**s).clone()),
        _ => Err(VmError::Malformed("malformed TimeZone object")),
    }
}

fn dispatch_datetime(
    name: &str,
    receiver: &Value,
    mut args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected DateTime receiver"));
    };
    let (epoch, zone) = {
        let locked = lock(&obj);
        let epoch = match locked.fields.get("__epoch__") {
            Some(Value::Int(e)) => *e,
            _ => return Err(VmError::Malformed("malformed DateTime object")),
        };
        let zone = match locked.fields.get("__zone__") {
            Some(Value::Str(z)) => (**z).clone(),
            _ => return Err(VmError::Malformed("malformed DateTime object")),
        };
        (epoch, zone)
    };
    match name {
        "getTimeZone" => Ok(Some(new_timezone_object(zone))),
        "withTimeZone" => Ok(Some(new_datetime_object(
            epoch,
            timezone_id(&expect_object(&mut args)?)?,
        ))),
        "toUtc" => Ok(Some(new_datetime_object(epoch, "UTC".to_string()))),
        "format" => {
            let pattern = expect_str(&mut args)?;
            let offset = crate::mini_tz::zone_offset_seconds(&zone, epoch)
                .map_err(|e| throw_native("IllegalArgumentException", e))?;
            Ok(Some(Value::Str(Arc::new(crate::mini_tz::format_datetime(
                epoch, offset, &pattern,
            )))))
        }
        "getYear" | "getMonth" | "getDay" | "getHour" | "getMinute" | "getSecond" => {
            let offset = crate::mini_tz::zone_offset_seconds(&zone, epoch)
                .map_err(|e| throw_native("IllegalArgumentException", e))?;
            let (y, mo, d, hh, mi, ss) = crate::mini_tz::epoch_to_local(epoch, offset);
            let v = match name {
                "getYear" => y,
                "getMonth" => mo as i64,
                "getDay" => d as i64,
                "getHour" => hh as i64,
                "getMinute" => mi as i64,
                _ => ss as i64, // "getSecond"
            };
            Ok(Some(Value::Int(v)))
        }
        _ => Err(VmError::MethodNotFound(format!(
            "system.time.DateTime.{name}"
        ))),
    }
}

fn dispatch_timezone(
    name: &str,
    receiver: &Value,
    mut args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected TimeZone receiver"));
    };
    let id = match lock(&obj).fields.get("__id__") {
        Some(Value::Str(s)) => (**s).clone(),
        _ => return Err(VmError::Malformed("malformed TimeZone object")),
    };
    match name {
        "getId" => Ok(Some(Value::Str(Arc::new(id)))),
        "getOffsetMinutes" => {
            let at = expect_object(&mut args)?;
            let Value::Object(at_obj) = &at else {
                return Err(VmError::Malformed(
                    "expected DateTime argument to native call",
                ));
            };
            let epoch = match lock(at_obj).fields.get("__epoch__") {
                Some(Value::Int(e)) => *e,
                _ => return Err(VmError::Malformed("malformed DateTime object")),
            };
            let offset = crate::mini_tz::zone_offset_seconds(&id, epoch)
                .map_err(|e| throw_native("IllegalArgumentException", e))?;
            Ok(Some(Value::Int((offset / 60) as i64)))
        }
        _ => Err(VmError::MethodNotFound(format!(
            "system.time.TimeZone.{name}"
        ))),
    }
}

fn expect_object(args: &mut Vec<Value>) -> Result<Value, VmError> {
    match args.pop() {
        Some(v @ Value::Object(_)) => Ok(v),
        _ => Err(VmError::Malformed(
            "expected object argument to native call",
        )),
    }
}

/// stdlib.md § system.net.TcpListener/TcpStream/UdpSocket — real OS
/// sockets via `std::net`. Same shape as `system.io.FileHandle`: the
/// object only carries an `"__fd__"` index into a `Program`-level table
/// (`Program::{tcp_listeners,tcp_streams,udp_sockets}`), since the actual
/// `std::net` value can't live on a `Value::Object` field. `TcpListener`
/// and `UdpSocket` are constructible directly by user code (`new
/// system.net.TcpListener(...)`), so — like `system.Random` — they're
/// intercepted at `NEW`/`INVOKE_SPECIAL <construct>` in
/// `interpreter::exec_step` (`is_net_listener_class`/`is_net_udp_class`)
/// rather than built by a static factory. `TcpStream` is the opposite:
/// never constructed with `new`, only via the static
/// `TcpStream.connect(...)` (handled in `dispatch`, below) or
/// `TcpListener.accept()` (`dispatch_tcp_listener`) — both build the
/// object directly, the same way `File.open` builds a `FileHandle`.
pub fn is_net_listener_class(fqcn: &str) -> bool {
    fqcn == "system.net.TcpListener"
}

pub fn is_net_udp_class(fqcn: &str) -> bool {
    fqcn == "system.net.UdpSocket"
}

pub fn new_tcp_listener_object() -> Value {
    let mut fields = HashMap::new();
    fields.insert("__fd__".to_string(), Value::Int(-1));
    Value::Object(Arc::new(Mutex::new(Object::native(
        "system.net.TcpListener",
        fields,
    ))))
}

pub fn construct_tcp_listener(
    program: &Arc<Program>,
    receiver: &Value,
    args: Vec<Value>,
) -> Result<(), VmError> {
    let host = str_at(&args, 0)?;
    let port = int_at(&args, 1)?;
    let listener = std::net::TcpListener::bind((host.as_str(), port as u16))
        .map_err(|e| throw_native("IOException", format!("bind {host}:{port}: {e}")))?;
    let id = program.register_tcp_listener(listener);
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected TcpListener receiver"));
    };
    lock(&obj)
        .fields
        .insert("__fd__".to_string(), Value::Int(id));
    Ok(())
}

fn dispatch_tcp_listener(
    program: &Arc<Program>,
    name: &str,
    receiver: &Value,
    _args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected TcpListener receiver"));
    };
    let id = match lock(&obj).fields.get("__fd__") {
        Some(Value::Int(id)) => *id,
        _ => return Err(VmError::Malformed("malformed TcpListener object")),
    };
    match name {
        "close" => {
            program.close_tcp_listener(id);
            Ok(None)
        }
        "accept" => {
            let (stream, _addr) = program
                .with_tcp_listener(id, |l| l.accept())
                .ok_or_else(|| throw_native("IOException", "accept on a closed listener"))?
                .map_err(|e| throw_native("IOException", e.to_string()))?;
            let stream_id = program.register_tcp_stream(stream);
            let mut fields = HashMap::new();
            fields.insert("__fd__".to_string(), Value::Int(stream_id));
            Ok(Some(Value::Object(Arc::new(Mutex::new(Object::native(
                "system.net.TcpStream",
                fields,
            ))))))
        }
        _ => Err(VmError::MethodNotFound(format!(
            "system.net.TcpListener.{name}"
        ))),
    }
}

fn dispatch_tcp_stream(
    program: &Arc<Program>,
    name: &str,
    receiver: &Value,
    args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    use std::io::{Read, Write};

    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected TcpStream receiver"));
    };
    let id = match lock(&obj).fields.get("__fd__") {
        Some(Value::Int(id)) => *id,
        _ => return Err(VmError::Malformed("malformed TcpStream object")),
    };
    if name == "close" {
        program.close_tcp_stream(id);
        return Ok(None);
    }
    let closed = || throw_native("IOException", format!("{name} on a closed stream"));
    // Both `read` and `write` take `(byte[] data, int offset, int length)`
    // — same bounds-checking rule as `system.io.FileHandle` (stdlib.md).
    let Some(Value::Array(buffer)) = args.first().cloned() else {
        return Err(VmError::Malformed(
            "expected byte[] argument to native call",
        ));
    };
    let offset = int_at(&args, 1)?;
    let length = int_at(&args, 2)?;
    let buf_len = lock(&buffer).len() as i64;
    if offset < 0 || length < 0 || offset.checked_add(length).is_none_or(|end| end > buf_len) {
        return Err(throw_native(
            "IndexOutOfBoundsException",
            format!("offset {offset}, length {length}, buffer length {buf_len}"),
        ));
    }
    match name {
        "read" => {
            let mut tmp = vec![0u8; length as usize];
            let n = program
                .with_tcp_stream(id, |s| s.read(&mut tmp))
                .ok_or_else(closed)?
                .map_err(|e| throw_native("IOException", e.to_string()))?;
            let mut buf = lock(&buffer);
            for (i, byte) in tmp[..n].iter().enumerate() {
                buf[offset as usize + i] = Value::Byte(*byte);
            }
            Ok(Some(Value::Int(n as i64)))
        }
        "write" => {
            let data: Vec<u8> = lock(&buffer)[offset as usize..(offset + length) as usize]
                .iter()
                .map(|v| match v {
                    Value::Byte(b) => Ok(*b),
                    Value::Int(i) => Ok(*i as u8),
                    _ => Err(VmError::Malformed(
                        "expected byte[] argument to native call",
                    )),
                })
                .collect::<Result<_, _>>()?;
            program
                .with_tcp_stream(id, |s| s.write_all(&data))
                .ok_or_else(closed)?
                .map_err(|e| throw_native("IOException", e.to_string()))?;
            Ok(None)
        }
        _ => Err(VmError::MethodNotFound(format!(
            "system.net.TcpStream.{name}"
        ))),
    }
}

pub fn new_udp_socket_object() -> Value {
    let mut fields = HashMap::new();
    fields.insert("__fd__".to_string(), Value::Int(-1));
    Value::Object(Arc::new(Mutex::new(Object::native(
        "system.net.UdpSocket",
        fields,
    ))))
}

/// `construct()` takes no arguments and declares no `throws` (stdlib.md),
/// but still needs a real OS socket underneath so `send()` works without
/// an explicit `bind()` first — bound to an ephemeral port, which for a
/// local allocation essentially never fails; `bind(host, port)` later
/// swaps in a socket bound to the caller's chosen address (see
/// `Program::rebind_udp_socket`).
pub fn construct_udp_socket(program: &Arc<Program>, receiver: &Value) -> Result<(), VmError> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0")
        .map_err(|e| throw_native("IOException", format!("failed to create UDP socket: {e}")))?;
    let id = program.register_udp_socket(socket);
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected UdpSocket receiver"));
    };
    lock(&obj)
        .fields
        .insert("__fd__".to_string(), Value::Int(id));
    Ok(())
}

fn dispatch_udp_socket(
    program: &Arc<Program>,
    name: &str,
    receiver: &Value,
    args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected UdpSocket receiver"));
    };
    let id = match lock(&obj).fields.get("__fd__") {
        Some(Value::Int(id)) => *id,
        _ => return Err(VmError::Malformed("malformed UdpSocket object")),
    };
    match name {
        "close" => {
            program.close_udp_socket(id);
            Ok(None)
        }
        "bind" => {
            let host = str_at(&args, 0)?;
            let port = int_at(&args, 1)?;
            let socket = std::net::UdpSocket::bind((host.as_str(), port as u16))
                .map_err(|e| throw_native("IOException", format!("bind {host}:{port}: {e}")))?;
            program.rebind_udp_socket(id, socket);
            Ok(None)
        }
        "send" => {
            let host = str_at(&args, 0)?;
            let port = int_at(&args, 1)?;
            let Some(Value::Array(buffer)) = args.get(2).cloned() else {
                return Err(VmError::Malformed(
                    "expected byte[] argument to native call",
                ));
            };
            let data: Vec<u8> = lock(&buffer)
                .iter()
                .map(|v| match v {
                    Value::Byte(b) => Ok(*b),
                    Value::Int(i) => Ok(*i as u8),
                    _ => Err(VmError::Malformed(
                        "expected byte[] argument to native call",
                    )),
                })
                .collect::<Result<_, _>>()?;
            program
                .with_udp_socket(id, |s| s.send_to(&data, (host.as_str(), port as u16)))
                .ok_or_else(|| throw_native("IOException", "send on a closed socket"))?
                .map_err(|e| throw_native("IOException", e.to_string()))?;
            Ok(None)
        }
        // No offset/length (stdlib.md): fills from index 0, truncating a
        // datagram larger than the buffer (`UdpSocket::recv`'s own
        // behavior on a too-small buffer already matches — the excess is
        // discarded, not an error).
        "receive" => {
            let Some(Value::Array(buffer)) = args.first().cloned() else {
                return Err(VmError::Malformed(
                    "expected byte[] argument to native call",
                ));
            };
            let buf_len = lock(&buffer).len();
            let mut tmp = vec![0u8; buf_len];
            let n = program
                .with_udp_socket(id, |s| s.recv(&mut tmp))
                .ok_or_else(|| throw_native("IOException", "receive on a closed socket"))?
                .map_err(|e| throw_native("IOException", e.to_string()))?;
            let mut buf = lock(&buffer);
            for (i, byte) in tmp[..n].iter().enumerate() {
                buf[i] = Value::Byte(*byte);
            }
            Ok(Some(Value::Int(n as i64)))
        }
        _ => Err(VmError::MethodNotFound(format!(
            "system.net.UdpSocket.{name}"
        ))),
    }
}

/// stdlib.md § system.thread.Thread/Mutex/Semaphore — vm.md § Threading
/// model: "each `system.thread.Thread` instance corresponds to a separate
/// VM thread". Backed by real `std::thread::spawn`, which is exactly why
/// `Value` had to move from `Rc`/`RefCell` to `Arc`/`Mutex` (see
/// `crate::value`'s doc comment) — a spawned thread needs `'static`,
/// `Send` captures, and heap objects (including a captured closure's own
/// fields) really are shared across the two threads afterwards.
///
/// All three are constructed directly by user code (`new
/// system.thread.Thread(...)` etc.), so — like `system.Random` — they're
/// intercepted at `NEW`/`INVOKE_SPECIAL <construct>` in
/// `interpreter::exec_step`, keyed by exact class name (never mangled).
/// `Mutex`/`Semaphore` are both just a bounded counter under the hood
/// (`Program::Counter`, built on `Condvar` rather than a `MutexGuard` —
/// see that type's doc comment for why): a `Mutex` is a `Counter` capped at
/// 1 (locked ⇔ count is `0`), a `Semaphore` is the same counter uncapped.
pub fn is_thread_class(fqcn: &str) -> bool {
    fqcn == "system.thread.Thread"
}

pub fn is_mutex_class(fqcn: &str) -> bool {
    fqcn == "system.thread.Mutex"
}

pub fn is_semaphore_class(fqcn: &str) -> bool {
    fqcn == "system.thread.Semaphore"
}

pub fn new_thread_object() -> Value {
    let mut fields = HashMap::new();
    fields.insert("__tid__".to_string(), Value::Int(-1));
    fields.insert("__task__".to_string(), Value::Null);
    Value::Object(Arc::new(Mutex::new(Object::native(
        "system.thread.Thread",
        fields,
    ))))
}

pub fn new_mutex_object() -> Value {
    let mut fields = HashMap::new();
    fields.insert("__mid__".to_string(), Value::Int(-1));
    Value::Object(Arc::new(Mutex::new(Object::native(
        "system.thread.Mutex",
        fields,
    ))))
}

pub fn new_semaphore_object() -> Value {
    let mut fields = HashMap::new();
    fields.insert("__sid__".to_string(), Value::Int(-1));
    Value::Object(Arc::new(Mutex::new(Object::native(
        "system.thread.Semaphore",
        fields,
    ))))
}

/// `Thread(() => void task)` — just stashes the closure; the thread isn't
/// actually spawned (and doesn't occupy a `Program::threads` slot) until
/// `start()` (see `dispatch_thread`).
pub fn construct_thread(receiver: &Value, mut args: Vec<Value>) -> Result<(), VmError> {
    let task = args.pop().ok_or(VmError::Malformed(
        "expected closure argument to native call",
    ))?;
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected Thread receiver"));
    };
    lock(&obj).fields.insert("__task__".to_string(), task);
    Ok(())
}

pub fn construct_mutex(program: &Arc<Program>, receiver: &Value) -> Result<(), VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected Mutex receiver"));
    };
    let id = program.register_mutex();
    lock(&obj)
        .fields
        .insert("__mid__".to_string(), Value::Int(id));
    Ok(())
}

pub fn construct_semaphore(
    program: &Arc<Program>,
    receiver: &Value,
    mut args: Vec<Value>,
) -> Result<(), VmError> {
    let initial = expect_int(&mut args)?;
    if initial < 0 {
        return Err(throw_native(
            "IllegalArgumentException",
            "initial count must not be negative",
        ));
    }
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected Semaphore receiver"));
    };
    let id = program.register_semaphore(initial);
    lock(&obj)
        .fields
        .insert("__sid__".to_string(), Value::Int(id));
    Ok(())
}

/// Resolves and calls a closure value's synthetic `invoke` method (see
/// `nl_codegen::closure`) with `args` — unlike `INVOKE_CLOSURE`'s bytecode
/// path there is no method-ref descriptor to match against, but a name-only
/// lookup (`Module::find_method`) is unambiguous: a closure's synthetic
/// class has exactly one method. Shared by `Thread`'s zero-arg task
/// (`invoke_task`) and the array/`Map` methods that accept native callbacks
/// (`map`/`filter`/`forEach`/`sort`/`find`, `Map.forEach` — see
/// `dispatch_array`).
fn invoke_closure(
    program: &Arc<Program>,
    closure: &Value,
    args: Vec<Value>,
) -> Result<Value, VmError> {
    let Value::Object(obj) = closure else {
        return Err(VmError::Malformed("expected closure receiver"));
    };
    let class_name = lock(obj).class_name.clone();
    let module = program
        .get(&class_name)
        .ok_or_else(|| VmError::MethodNotFound(class_name.clone()))?;
    let method = module
        .find_method("invoke")
        .ok_or_else(|| VmError::MethodNotFound(format!("{class_name}.invoke")))?;
    let result = crate::interpreter::call_instance(program, module, method, closure.clone(), args)?;
    Ok(result.unwrap_or(Value::Null))
}

/// `Thread`'s task is always a zero-arg `() => void` closure.
fn invoke_task(program: &Arc<Program>, task: Value) -> Result<Option<Value>, VmError> {
    invoke_closure(program, &task, vec![]).map(Some)
}

fn dispatch_thread(
    program: &Arc<Program>,
    name: &str,
    receiver: &Value,
    mut args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected Thread receiver"));
    };
    match name {
        // Starting an already-started (or never-given-a-task) `Thread`
        // twice is a caller error stdlib.md just prohibits by contract
        // ("must not be called more than once") rather than one this VM
        // detects — treated as a no-op, consistent with e.g.
        // `FileHandle.close()` tolerating a redundant call.
        "start" => {
            let task = lock(&obj)
                .fields
                .get("__task__")
                .cloned()
                .unwrap_or(Value::Null);
            if task.is_null() {
                return Ok(None);
            }
            lock(&obj)
                .fields
                .insert("__task__".to_string(), Value::Null);
            let program_clone = Arc::clone(program);
            // `interpreter::run_frame` recurses natively (one Rust stack
            // frame per NL call frame — see `call_stack`'s module doc
            // comment), and `call_stack::MAX_CALL_DEPTH` is sized against
            // the *main* thread's stack. `std::thread::spawn`'s platform
            // default (commonly 2 MiB) is well under that — matching the
            // main thread's here (see `program::run_program`'s host process,
            // whose stack comes from the OS/`ulimit -s`, typically 8 MiB on
            // Linux) keeps `StackOverflowException` firing at the same NL
            // call depth regardless of which thread a program recurses on,
            // rather than segfaulting on a `system.thread.Thread` well
            // before the depth guard would trip.
            const THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;
            let handle = std::thread::Builder::new()
                .stack_size(THREAD_STACK_SIZE)
                .spawn(move || match invoke_task(&program_clone, task) {
                    Ok(_) => {}
                    // Same wording as the main thread's own unhandled-exception
                    // report (`program::run_program`) — no trailing newline
                    // either, for the same reason: a later write (from this
                    // thread or another) shouldn't inherit a blank line.
                    Err(VmError::Thrown(exc)) => {
                        program_clone.write_stderr(&format!(
                            "Unhandled exception: {}",
                            crate::program::describe_exception(&exc)
                        ));
                    }
                    Err(e) => program_clone.write_stderr(&format!("Unhandled exception: {e}")),
                })
                .expect("spawning an OS thread for system.thread.Thread");
            let tid = program.register_thread(handle);
            lock(&obj)
                .fields
                .insert("__tid__".to_string(), Value::Int(tid));
            Ok(None)
        }
        "join" if args.is_empty() => {
            if let Some(tid) = thread_id(&obj) {
                program.join_thread(tid);
            }
            Ok(None)
        }
        "join" => {
            let timeout_millis = expect_int(&mut args)?.max(0) as u64;
            let Some(tid) = thread_id(&obj) else {
                return Ok(Some(Value::Bool(true)));
            };
            // `std::thread::JoinHandle` has no timed join — polls
            // `is_finished()` instead, bounded by `timeout_millis`.
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_millis(timeout_millis);
            loop {
                if program.thread_is_finished(tid) {
                    program.join_thread(tid);
                    return Ok(Some(Value::Bool(true)));
                }
                if std::time::Instant::now() >= deadline {
                    return Ok(Some(Value::Bool(false)));
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        "isAlive" => match thread_id(&obj) {
            Some(tid) => Ok(Some(Value::Bool(!program.thread_is_finished(tid)))),
            None => Ok(Some(Value::Bool(false))),
        },
        _ => Err(VmError::MethodNotFound(format!(
            "system.thread.Thread.{name}"
        ))),
    }
}

fn thread_id(obj: &Arc<Mutex<Object>>) -> Option<i64> {
    match lock(obj).fields.get("__tid__") {
        Some(Value::Int(id)) if *id >= 0 => Some(*id),
        _ => None,
    }
}

fn dispatch_mutex(
    program: &Arc<Program>,
    name: &str,
    receiver: &Value,
    _args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected Mutex receiver"));
    };
    let id = match lock(&obj).fields.get("__mid__") {
        Some(Value::Int(id)) => *id,
        _ => return Err(VmError::Malformed("malformed Mutex object")),
    };
    let counter = program
        .mutex(id)
        .ok_or(VmError::Malformed("malformed Mutex object"))?;
    match name {
        "lock" => {
            counter.acquire();
            Ok(None)
        }
        "unlock" => {
            counter.release();
            Ok(None)
        }
        "tryLock" => Ok(Some(Value::Bool(counter.try_acquire()))),
        _ => Err(VmError::MethodNotFound(format!(
            "system.thread.Mutex.{name}"
        ))),
    }
}

fn dispatch_semaphore(
    program: &Arc<Program>,
    name: &str,
    receiver: &Value,
    _args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected Semaphore receiver"));
    };
    let id = match lock(&obj).fields.get("__sid__") {
        Some(Value::Int(id)) => *id,
        _ => return Err(VmError::Malformed("malformed Semaphore object")),
    };
    let counter = program
        .semaphore(id)
        .ok_or(VmError::Malformed("malformed Semaphore object"))?;
    match name {
        "acquire" => {
            counter.acquire();
            Ok(None)
        }
        "release" => {
            counter.release();
            Ok(None)
        }
        "tryAcquire" => Ok(Some(Value::Bool(counter.try_acquire()))),
        _ => Err(VmError::MethodNotFound(format!(
            "system.thread.Semaphore.{name}"
        ))),
    }
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
        fields.insert(
            "__data__".to_string(),
            Value::Array(Arc::new(Mutex::new(Vec::new()))),
        );
    } else {
        fields.insert(
            "__keys__".to_string(),
            Value::Array(Arc::new(Mutex::new(Vec::new()))),
        );
        fields.insert(
            "__values__".to_string(),
            Value::Array(Arc::new(Mutex::new(Vec::new()))),
        );
    }
    Value::Object(Arc::new(Mutex::new(Object::native(fqcn, fields))))
}

/// `Opcode::InvokeSpecial` on a native generic class's `<construct>`. Only
/// `system.List<T>(T[] initial)` does anything; `List()` and `Map()` leave
/// the empty fields `new_generic_object` already set up untouched.
pub fn construct_generic(
    receiver: &Value,
    fqcn: &str,
    mut args: Vec<Value>,
) -> Result<(), VmError> {
    if fqcn.starts_with("system.List<") {
        if let Some(Value::Array(initial)) = args.pop() {
            let data = list_data(receiver)?;
            lock(&data).extend(lock(&initial).iter().cloned());
        }
    }
    Ok(())
}

/// `Opcode::InvokeInstance` against a native generic class — dispatched by
/// the *receiver's* runtime class, same as `resolve_virtual` would for a
/// bytecode-backed class.
pub fn dispatch_instance(
    program: &Arc<Program>,
    fqcn: &str,
    name: &str,
    receiver: &Value,
    args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    if fqcn.starts_with("system.List<") {
        dispatch_list(program, name, receiver, args)
    } else {
        dispatch_map(program, name, receiver, args)
    }
}

/// specs.md § ValueEquatable interface / stdlib.md § system.Map,
/// `List.contains`: structural equality via `a.valueEquals(b)` when `a`'s
/// runtime class implements `ValueEquatable`, falling back to
/// `values_equal` (primitive/`string` value equality, reference identity
/// for everything else) otherwise. Only `a` (the stored key/element) is
/// checked — both sides are the same static type `K`/`T` at every call
/// site, so either both implement the interface or neither does.
fn equatable_equals(program: &Arc<Program>, a: &Value, b: &Value) -> Result<bool, VmError> {
    if let Value::Object(obj) = a {
        let class_name = lock(obj).class_name.clone();
        if is_instance_of(program, class_name.clone(), "ValueEquatable") {
            if let Some((module, method)) = resolve_virtual_by_name(program, &class_name, "valueEquals")
            {
                let result = call_instance(program, module, method, a.clone(), vec![b.clone()])?;
                return Ok(matches!(result, Some(Value::Bool(true))));
            }
        }
    }
    Ok(values_equal(a, b))
}

type ArrayRc = Arc<Mutex<Vec<Value>>>;

fn list_data(receiver: &Value) -> Result<ArrayRc, VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected List receiver"));
    };
    match lock(&obj).fields.get("__data__") {
        Some(Value::Array(a)) => Ok(Arc::clone(a)),
        _ => Err(VmError::Malformed("malformed List object")),
    }
}

fn dispatch_list(
    program: &Arc<Program>,
    name: &str,
    receiver: &Value,
    mut args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let data = list_data(receiver)?;
    match name {
        "size" => Ok(Some(Value::Int(lock(&data).len() as i64))),
        "get" => {
            let idx = expect_int(&mut args)?;
            let d = lock(&data);
            if idx < 0 || idx as usize >= d.len() {
                return Err(throw_native(
                    "IndexOutOfBoundsException",
                    format!("index {idx}, length {}", d.len()),
                ));
            }
            Ok(Some(d[idx as usize].clone()))
        }
        "set" => {
            let value = args
                .pop()
                .ok_or(VmError::Malformed("missing value argument"))?;
            let idx = expect_int(&mut args)?;
            let mut d = lock(&data);
            if idx < 0 || idx as usize >= d.len() {
                return Err(throw_native(
                    "IndexOutOfBoundsException",
                    format!("index {idx}, length {}", d.len()),
                ));
            }
            d[idx as usize] = value;
            Ok(None)
        }
        "pushBack" | "add" => {
            let value = args
                .pop()
                .ok_or(VmError::Malformed("missing value argument"))?;
            lock(&data).push(value);
            Ok(None)
        }
        "pushFront" => {
            let value = args
                .pop()
                .ok_or(VmError::Malformed("missing value argument"))?;
            lock(&data).insert(0, value);
            Ok(None)
        }
        "popBack" => match lock(&data).pop() {
            Some(v) => Ok(Some(v)),
            None => Err(throw_native(
                "IndexOutOfBoundsException",
                "popBack on empty list",
            )),
        },
        "popFront" => {
            let mut d = lock(&data);
            if d.is_empty() {
                return Err(throw_native(
                    "IndexOutOfBoundsException",
                    "popFront on empty list",
                ));
            }
            Ok(Some(d.remove(0)))
        }
        "remove" => {
            let idx = expect_int(&mut args)?;
            let mut d = lock(&data);
            if idx < 0 || idx as usize >= d.len() {
                return Err(throw_native(
                    "IndexOutOfBoundsException",
                    format!("index {idx}, length {}", d.len()),
                ));
            }
            Ok(Some(d.remove(idx as usize)))
        }
        "contains" => {
            let value = args
                .pop()
                .ok_or(VmError::Malformed("missing value argument"))?;
            // Snapshot before calling back into `equatable_equals` (which
            // may run user NL code, `valueEquals`) rather than holding
            // `data`'s lock across it — same not-across-a-call-boundary
            // rule as `interpreter::exec_step`'s `SET_FIELD` comment.
            let snapshot = lock(&data).clone();
            let mut found = false;
            for v in &snapshot {
                if equatable_equals(program, v, &value)? {
                    found = true;
                    break;
                }
            }
            Ok(Some(Value::Bool(found)))
        }
        _ => Err(VmError::MethodNotFound(format!("system.List.{name}"))),
    }
}

fn map_storage(receiver: &Value) -> Result<(ArrayRc, ArrayRc), VmError> {
    let Value::Object(obj) = receiver else {
        return Err(VmError::Malformed("expected Map receiver"));
    };
    let obj = lock(&obj);
    match (obj.fields.get("__keys__"), obj.fields.get("__values__")) {
        (Some(Value::Array(k)), Some(Value::Array(v))) => Ok((Arc::clone(k), Arc::clone(v))),
        _ => Err(VmError::Malformed("malformed Map object")),
    }
}

/// `get`/`set`/`remove`/`has`'s shared key lookup — see `equatable_equals`.
/// Snapshots `keys` first rather than holding its lock across the
/// (potentially user-code-calling) equality check.
fn find_key_index(
    program: &Arc<Program>,
    keys: &ArrayRc,
    key: &Value,
) -> Result<Option<usize>, VmError> {
    let snapshot = lock(keys).clone();
    for (i, k) in snapshot.iter().enumerate() {
        if equatable_equals(program, k, key)? {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

fn dispatch_map(
    program: &Arc<Program>,
    name: &str,
    receiver: &Value,
    mut args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let (keys, values) = map_storage(receiver)?;
    match name {
        "size" => Ok(Some(Value::Int(lock(&keys).len() as i64))),
        "get" => {
            let key = args
                .pop()
                .ok_or(VmError::Malformed("missing key argument"))?;
            let idx = find_key_index(program, &keys, &key)?;
            Ok(Some(match idx {
                Some(i) => lock(&values)[i].clone(),
                None => Value::Null,
            }))
        }
        "set" => {
            let value = args
                .pop()
                .ok_or(VmError::Malformed("missing value argument"))?;
            let key = args
                .pop()
                .ok_or(VmError::Malformed("missing key argument"))?;
            let idx = find_key_index(program, &keys, &key)?;
            match idx {
                Some(i) => lock(&values)[i] = value,
                None => {
                    lock(&keys).push(key);
                    lock(&values).push(value);
                }
            }
            Ok(None)
        }
        "remove" => {
            let key = args
                .pop()
                .ok_or(VmError::Malformed("missing key argument"))?;
            let idx = find_key_index(program, &keys, &key)?;
            match idx {
                Some(i) => {
                    lock(&keys).remove(i);
                    lock(&values).remove(i);
                    Ok(Some(Value::Bool(true)))
                }
                None => Ok(Some(Value::Bool(false))),
            }
        }
        "has" => {
            let key = args
                .pop()
                .ok_or(VmError::Malformed("missing key argument"))?;
            Ok(Some(Value::Bool(find_key_index(program, &keys, &key)?.is_some())))
        }
        "keys" => Ok(Some(Value::Array(Arc::new(Mutex::new(
            lock(&keys).clone(),
        ))))),
        "values" => Ok(Some(Value::Array(Arc::new(Mutex::new(
            lock(&values).clone(),
        ))))),
        // stdlib.md § system.MapEntry — result objects with two public
        // fields, classed under the matching mangled `MapEntry`
        // instantiation (`"system.Map<string, int>"` ->
        // `"system.MapEntry<string, int>"`). Iteration order == `keys()`'s,
        // as the spec requires ("consistent").
        "entries" => {
            let Value::Object(obj) = receiver else {
                return Err(VmError::Malformed("expected Map receiver"));
            };
            let entry_class = format!(
                "system.MapEntry<{}",
                &lock(&obj).class_name["system.Map<".len()..]
            );
            let entries: Vec<Value> = lock(&keys)
                .iter()
                .zip(lock(&values).iter())
                .map(|(k, v)| {
                    let mut fields = HashMap::new();
                    fields.insert("key".to_string(), k.clone());
                    fields.insert("value".to_string(), v.clone());
                    Value::Object(Arc::new(Mutex::new(Object::native(
                        entry_class.clone(),
                        fields,
                    ))))
                })
                .collect();
            Ok(Some(Value::Array(Arc::new(Mutex::new(entries)))))
        }
        // stdlib.md § system.Map — "Invokes `f` for each key-value pair",
        // same iteration order as `keys()`/`entries()`. Snapshots both
        // arrays first so a callback that mutates the map mid-iteration
        // (e.g. `remove`) can't shift indices out from under this loop.
        "forEach" => {
            let f = args
                .pop()
                .ok_or(VmError::Malformed("missing callback argument"))?;
            let ks = lock(&keys).clone();
            let vs = lock(&values).clone();
            for (k, v) in ks.into_iter().zip(vs) {
                invoke_closure(program, &f, vec![k, v])?;
            }
            Ok(None)
        }
        _ => Err(VmError::MethodNotFound(format!("system.Map.{name}"))),
    }
}

/// `Opcode::InvokeInstance` on a `Value::Array` receiver — specs.md §
/// Arrays, Built-in methods / vm.md § Standard library binding: "the other
/// six methods are invoked via `INVOKE_INSTANCE` on the array reference and
/// dispatched to native implementations by the VM. Methods that accept
/// callbacks ... receive a closure object as an argument; the native
/// implementation calls `INVOKE_CLOSURE` internally for each element."
/// (`length` is `ARRAY_LENGTH`, a dedicated opcode, and never reaches here.)
///
/// Each callback-taking method snapshots the backing `Vec` up front
/// (`lock(arr).clone()`) rather than holding the lock while calling back
/// into NL code — the callback may itself touch the same array (e.g.
/// `arr.forEach((v) => arr.pushBack(v))`), which would deadlock on a
/// re-entrant lock attempt otherwise.
pub fn dispatch_array(
    program: &Arc<Program>,
    name: &str,
    receiver: &Value,
    mut args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let Value::Array(arr) = receiver else {
        return Err(VmError::Malformed("expected array receiver"));
    };
    match name {
        "slice" => {
            let end = expect_int(&mut args)?;
            let start = expect_int(&mut args)?;
            let items = lock(arr).clone();
            let len = items.len() as i64;
            let start = start.clamp(0, len) as usize;
            let end = end.clamp(0, len) as usize;
            let sliced = if start < end {
                items[start..end].to_vec()
            } else {
                Vec::new()
            };
            Ok(Some(Value::Array(Arc::new(Mutex::new(sliced)))))
        }
        "map" => {
            let f = args
                .pop()
                .ok_or(VmError::Malformed("missing callback argument"))?;
            let items = lock(arr).clone();
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(invoke_closure(program, &f, vec![item])?);
            }
            Ok(Some(Value::Array(Arc::new(Mutex::new(out)))))
        }
        "filter" => {
            let f = args
                .pop()
                .ok_or(VmError::Malformed("missing callback argument"))?;
            let items = lock(arr).clone();
            let mut out = Vec::new();
            for item in items {
                if invoke_closure(program, &f, vec![item.clone()])?
                    .as_bool()
                    .unwrap_or(false)
                {
                    out.push(item);
                }
            }
            Ok(Some(Value::Array(Arc::new(Mutex::new(out)))))
        }
        "forEach" => {
            let f = args
                .pop()
                .ok_or(VmError::Malformed("missing callback argument"))?;
            let items = lock(arr).clone();
            for item in items {
                invoke_closure(program, &f, vec![item])?;
            }
            Ok(None)
        }
        "sort" => {
            let f = args
                .pop()
                .ok_or(VmError::Malformed("missing callback argument"))?;
            let mut items = lock(arr).clone();
            insertion_sort_by_closure(program, &mut items, &f)?;
            *lock(arr) = items;
            Ok(None)
        }
        "find" => {
            let f = args
                .pop()
                .ok_or(VmError::Malformed("missing callback argument"))?;
            let items = lock(arr).clone();
            for item in items {
                if invoke_closure(program, &f, vec![item.clone()])?
                    .as_bool()
                    .unwrap_or(false)
                {
                    return Ok(Some(item));
                }
            }
            Ok(Some(Value::Null))
        }
        _ => Err(VmError::MethodNotFound(format!("array.{name}"))),
    }
}

/// `array.sort((T a, T b) => int compare)` — specs.md: "negative if a < b,
/// zero if equal, positive if a > b". Plain insertion sort (O(n²)) rather
/// than `slice::sort_by`, which can't propagate a `Result` from a fallible
/// comparator (`invoke_closure` may throw an NL exception, e.g. a bug in the
/// callback itself) — same small-test-size tradeoff already made for
/// `system.Map`'s O(n) key lookup.
fn insertion_sort_by_closure(
    program: &Arc<Program>,
    items: &mut [Value],
    f: &Value,
) -> Result<(), VmError> {
    for i in 1..items.len() {
        let mut j = i;
        while j > 0 {
            let cmp = invoke_closure(program, f, vec![items[j - 1].clone(), items[j].clone()])?;
            if cmp.as_int().unwrap_or(0) > 0 {
                items.swap(j - 1, j);
                j -= 1;
            } else {
                break;
            }
        }
    }
    Ok(())
}
