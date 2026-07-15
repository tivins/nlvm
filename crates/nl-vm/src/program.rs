use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use nl_bytecode::{Module, MethodDescriptor};

use crate::error::VmError;
use crate::interpreter::call_static;
use crate::value::{lock, Value};

/// A counting synchronization primitive shared by `system.thread.Mutex`
/// (as a 0/1 lock: `bool` doubles as "locked") and `system.thread.Semaphore`
/// (as a bounded counter). Built on `Condvar` rather than holding a
/// `MutexGuard` across the `lock()`/`unlock()` call boundary — a guard
/// can't outlive the single native call that acquires it, but the *logical*
/// lock must stay held across arbitrarily many other native calls in
/// between (vm.md § Threading model's mutex happens-before guarantee is
/// about `lock()`/`unlock()` call pairs, not Rust's own borrow scopes).
pub(crate) struct Counter {
    state: Mutex<i64>,
    condvar: Condvar,
}

impl Counter {
    fn new(initial: i64) -> Arc<Counter> {
        Arc::new(Counter { state: Mutex::new(initial), condvar: Condvar::new() })
    }

    /// Blocks while the count is `0`, then decrements it by one.
    pub(crate) fn acquire(&self) {
        let mut guard = lock(&self.state);
        while *guard == 0 {
            guard = self.condvar.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
        *guard -= 1;
    }

    pub(crate) fn try_acquire(&self) -> bool {
        let mut guard = lock(&self.state);
        if *guard == 0 {
            false
        } else {
            *guard -= 1;
            true
        }
    }

    pub(crate) fn release(&self) {
        let mut guard = lock(&self.state);
        *guard += 1;
        self.condvar.notify_one();
    }
}

/// A linked program: every module that will be executed together, keyed by
/// fully-qualified class name. Built once per run so cross-file references
/// (`new`, field access, instance/static method calls — see
/// `nl_bytecode::ConstantPoolEntry::{Class,FieldRef,MethodRef}`) resolve to
/// the right module instead of assuming everything lives in one file.
///
/// Wrapped in `Arc` by every entry point (`run_program`, `native::Thread`'s
/// `start()`) rather than borrowed: a spawned `system.thread.Thread` runs
/// on a real OS thread (`std::thread::spawn`, which requires `'static`
/// captures), so it needs to *own* a handle to the program, not merely
/// borrow one tied to the spawning frame's stack.
pub struct Program {
    modules: HashMap<String, Module>,
    /// Accumulated output from native `system.Out`/`system.Err` calls (see
    /// `crate::native`) — `Program` is shared across every call frame *and*
    /// every thread, so these are interior-mutable rather than threaded
    /// explicitly through `call_static`/`call_instance`/`run_frame`.
    stdout: Mutex<String>,
    stderr: Mutex<String>,
    /// Open files backing `system.io.FileHandle` objects (see
    /// `crate::native`): a handle object only carries an index into this
    /// table, and `close()` clears the slot (making the index permanently
    /// dead — stdlib.md: "After the handle has been closed, any call to
    /// read, readLine, write, or flush throws IOException").
    file_handles: Mutex<Vec<Option<std::fs::File>>>,
    /// Same pattern as `file_handles`, one table per `system.net.*` handle
    /// class (see `crate::native`'s network section). Kept as three
    /// separate tables rather than one enum table since each handle class
    /// only ever indexes its own.
    tcp_listeners: Mutex<Vec<Option<std::net::TcpListener>>>,
    tcp_streams: Mutex<Vec<Option<std::net::TcpStream>>>,
    udp_sockets: Mutex<Vec<Option<std::net::UdpSocket>>>,
    /// Backing store for `system.thread.Thread` — a thread object only
    /// carries an index into this table (`"__tid__"`, allocated by
    /// `start()`, not `NEW`, since an unstarted `Thread` shouldn't occupy a
    /// slot). `join()` takes the handle out (`Option::take`); a slot left
    /// `None` after that means "already joined", matching `FileHandle`'s
    /// close-is-terminal pattern.
    threads: Mutex<Vec<Option<JoinHandle<()>>>>,
    /// Backing store for `system.thread.Mutex` (`"__mid__"`) — modeled as a
    /// `Counter` capped at 1 (`lock`/`unlock`/`tryLock` treat `0` as locked,
    /// `1` as unlocked).
    thread_mutexes: Mutex<Vec<Option<Arc<Counter>>>>,
    /// Backing store for `system.thread.Semaphore` (`"__sid__"`).
    thread_semaphores: Mutex<Vec<Option<Arc<Counter>>>>,
}

impl Program {
    pub fn new(modules: Vec<Module>) -> Self {
        let mut map = HashMap::with_capacity(modules.len());
        for module in modules {
            if let Some(name) = module.this_class_name() {
                map.insert(name.to_string(), module);
            }
        }
        Program {
            modules: map,
            stdout: Mutex::new(String::new()),
            stderr: Mutex::new(String::new()),
            file_handles: Mutex::new(Vec::new()),
            tcp_listeners: Mutex::new(Vec::new()),
            tcp_streams: Mutex::new(Vec::new()),
            udp_sockets: Mutex::new(Vec::new()),
            threads: Mutex::new(Vec::new()),
            thread_mutexes: Mutex::new(Vec::new()),
            thread_semaphores: Mutex::new(Vec::new()),
        }
    }

    pub fn get(&self, fqcn: &str) -> Option<&Module> {
        self.modules.get(fqcn)
    }

    pub fn find_main(&self) -> Option<(&Module, &MethodDescriptor)> {
        self.modules.values().find_map(|m| m.find_method("main").map(|meth| (m, meth)))
    }

    pub fn write_stdout(&self, s: &str) {
        lock(&self.stdout).push_str(s);
    }

    pub fn write_stderr(&self, s: &str) {
        lock(&self.stderr).push_str(s);
    }

    pub fn register_file(&self, file: std::fs::File) -> i64 {
        let mut handles = lock(&self.file_handles);
        handles.push(Some(file));
        (handles.len() - 1) as i64
    }

    /// Idempotent, like `FileHandle.close()` itself (stdlib.md) — closing an
    /// already-closed or unknown id is a no-op. Dropping the `File` closes it.
    pub fn close_file(&self, id: i64) {
        if let Some(slot) = lock(&self.file_handles).get_mut(id as usize) {
            *slot = None;
        }
    }

    /// Runs `f` on the open file for `id`, or `None` if the id is unknown
    /// or the handle was closed (the caller turns that into `IOException`).
    pub fn with_file<R>(&self, id: i64, f: impl FnOnce(&mut std::fs::File) -> R) -> Option<R> {
        let mut handles = lock(&self.file_handles);
        handles.get_mut(id as usize)?.as_mut().map(f)
    }

    pub fn register_tcp_listener(&self, listener: std::net::TcpListener) -> i64 {
        let mut listeners = lock(&self.tcp_listeners);
        listeners.push(Some(listener));
        (listeners.len() - 1) as i64
    }

    pub fn close_tcp_listener(&self, id: i64) {
        if let Some(slot) = lock(&self.tcp_listeners).get_mut(id as usize) {
            *slot = None;
        }
    }

    pub fn with_tcp_listener<R>(&self, id: i64, f: impl FnOnce(&mut std::net::TcpListener) -> R) -> Option<R> {
        let mut listeners = lock(&self.tcp_listeners);
        listeners.get_mut(id as usize)?.as_mut().map(f)
    }

    pub fn register_tcp_stream(&self, stream: std::net::TcpStream) -> i64 {
        let mut streams = lock(&self.tcp_streams);
        streams.push(Some(stream));
        (streams.len() - 1) as i64
    }

    pub fn close_tcp_stream(&self, id: i64) {
        if let Some(slot) = lock(&self.tcp_streams).get_mut(id as usize) {
            *slot = None;
        }
    }

    pub fn with_tcp_stream<R>(&self, id: i64, f: impl FnOnce(&mut std::net::TcpStream) -> R) -> Option<R> {
        let mut streams = lock(&self.tcp_streams);
        streams.get_mut(id as usize)?.as_mut().map(f)
    }

    pub fn register_udp_socket(&self, socket: std::net::UdpSocket) -> i64 {
        let mut sockets = lock(&self.udp_sockets);
        sockets.push(Some(socket));
        (sockets.len() - 1) as i64
    }

    pub fn close_udp_socket(&self, id: i64) {
        if let Some(slot) = lock(&self.udp_sockets).get_mut(id as usize) {
            *slot = None;
        }
    }

    pub fn with_udp_socket<R>(&self, id: i64, f: impl FnOnce(&mut std::net::UdpSocket) -> R) -> Option<R> {
        let mut sockets = lock(&self.udp_sockets);
        sockets.get_mut(id as usize)?.as_mut().map(f)
    }

    /// `UdpSocket.bind(host, port)` re-binds the *same* handle to a chosen
    /// address — `construct()` already gave it an OS socket (an ephemeral
    /// port, so `send()` works without an explicit `bind()`), and `std`
    /// has no in-place rebind, so this swaps the slot for a freshly bound
    /// socket instead of allocating a new id/object.
    pub fn rebind_udp_socket(&self, id: i64, socket: std::net::UdpSocket) {
        if let Some(slot) = lock(&self.udp_sockets).get_mut(id as usize) {
            *slot = Some(socket);
        }
    }

    pub(crate) fn register_thread(&self, handle: JoinHandle<()>) -> i64 {
        let mut threads = lock(&self.threads);
        threads.push(Some(handle));
        (threads.len() - 1) as i64
    }

    pub(crate) fn thread_is_finished(&self, id: i64) -> bool {
        match lock(&self.threads).get(id as usize) {
            Some(Some(handle)) => handle.is_finished(),
            _ => true,
        }
    }

    /// Takes the handle out (idempotent: a slot left empty by a previous
    /// `join()` reads back as "already finished", `Ok(())`). A genuine Rust
    /// panic inside the task (a VM bug, not an NL-level exception — those
    /// are caught and reported to stderr *inside* the task, see
    /// `crate::native::dispatch_thread`) is swallowed here rather than
    /// re-panicking the joining thread, matching vm.md's destructor
    /// contract stance that one component's failure shouldn't cascade.
    pub(crate) fn join_thread(&self, id: i64) {
        let handle = lock(&self.threads).get_mut(id as usize).and_then(Option::take);
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    pub(crate) fn register_mutex(&self) -> i64 {
        let mut mutexes = lock(&self.thread_mutexes);
        mutexes.push(Some(Counter::new(1)));
        (mutexes.len() - 1) as i64
    }

    pub(crate) fn mutex(&self, id: i64) -> Option<Arc<Counter>> {
        lock(&self.thread_mutexes).get(id as usize).and_then(Clone::clone)
    }

    pub(crate) fn register_semaphore(&self, initial: i64) -> i64 {
        let mut semaphores = lock(&self.thread_semaphores);
        semaphores.push(Some(Counter::new(initial)));
        (semaphores.len() - 1) as i64
    }

    pub(crate) fn semaphore(&self, id: i64) -> Option<Arc<Counter>> {
        lock(&self.thread_semaphores).get(id as usize).and_then(Clone::clone)
    }
}

pub struct RunOutcome {
    pub exit_code: i32,
    /// Everything written via `system.Out.print`/`println` (see `crate::native`).
    pub stdout: String,
    /// Everything written via `system.Err.print`/`println`, plus the
    /// unhandled-exception message if any (see § Program startup, step 7).
    pub stderr: String,
}

/// Program startup — see nlvm-specs/docs/vm.md § Program startup.
///
/// Step 7 ("when main returns, ... exit") is taken literally: any
/// `system.thread.Thread` still running when `main` returns is abandoned,
/// not waited for (there is no "non-daemon thread" concept in the spec).
/// A conformant NL program that wants to wait for its worker threads calls
/// `join()` itself before returning from `main`, as every home-grown test
/// in this phase does.
pub fn run_program(modules: &[Module], program_args: &[String]) -> RunOutcome {
    let program = Arc::new(Program::new(modules.to_vec()));

    let Some((main_module, main)) = program.find_main() else {
        return RunOutcome {
            exit_code: 1,
            stdout: String::new(),
            stderr: format!("{}", VmError::NoMain),
        };
    };

    let args_array = Value::Array(Arc::new(Mutex::new(
        program_args
            .iter()
            .map(|s| Value::Str(Arc::new(s.clone())))
            .collect(),
    )));

    let result = call_static(&program, main_module, main, vec![args_array]);
    let stdout = lock(&program.stdout).clone();
    let mut stderr = lock(&program.stderr).clone();
    match result {
        Ok(Some(Value::Int(code))) => RunOutcome { exit_code: code as i32, stdout, stderr },
        Ok(_) => RunOutcome { exit_code: 0, stdout, stderr },
        Err(VmError::Thrown(exc)) => {
            append_line(&mut stderr, &format!("Unhandled exception: {}", describe_exception(&exc)));
            RunOutcome { exit_code: 1, stdout, stderr }
        }
        // `system.ps.Process.exit(code)` — see `VmError::Exit`'s doc
        // comment. Not an error at all from `run_program`'s point of view,
        // just an early, uncatchable short-circuit of `main`'s own return
        // value.
        Err(VmError::Exit(code)) => RunOutcome { exit_code: code, stdout, stderr },
        Err(e) => {
            append_line(&mut stderr, &format!("Unhandled exception: {e}"));
            RunOutcome { exit_code: 1, stdout, stderr }
        }
    }
}

fn append_line(buf: &mut String, line: &str) {
    if !buf.is_empty() && !buf.ends_with('\n') {
        buf.push('\n');
    }
    buf.push_str(line);
}

/// `vm.md § Throw and stack unwinding`, step 5: "the VM prints the
/// exception message ... to stderr". Renders as `ClassName: message` (or
/// bare `ClassName` if `message` is absent/not a string) — matches the
/// implicit-exception wording already used by e.g. `IndexOutOfBoundsException`.
pub(crate) fn describe_exception(exc: &Value) -> String {
    let Value::Object(obj) = exc else {
        return exc.to_display_string();
    };
    let obj = lock(obj);
    match obj.fields.get("message") {
        Some(Value::Str(s)) if !s.is_empty() => format!("{}: {s}", obj.class_name),
        _ => obj.class_name.clone(),
    }
}
