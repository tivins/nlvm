#[derive(Debug, thiserror::Error)]
pub enum VmError {
    #[error(transparent)]
    Bytecode(#[from] nl_bytecode::BytecodeError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unknown opcode byte {0}")]
    UnknownOpcode(u8),
    #[error("method '{0}' not found")]
    MethodNotFound(String),
    #[error("no 'main' method in module")]
    NoMain,
    /// An exception object propagating up the call stack — vm.md § Throw
    /// and stack unwinding. Carries the `Value::Object` itself (its
    /// `class_name`/`message` field) so `run_frame` can match it against
    /// each frame's exception table and, if unhandled anywhere, `run_program`
    /// can report it. Explicit `throw` and implicit exceptions (division by
    /// zero, null dereference, out-of-bounds access) both produce this.
    #[error("unhandled exception: {0:?}")]
    Thrown(crate::value::Value),
    #[error("unsupported opcode in this milestone: {0}")]
    Unsupported(String),
    #[error("malformed bytecode: {0}")]
    Malformed(&'static str),
    /// `system.ps.Process.exit(code)` (stdlib.md: "Terminal statement: does
    /// not return"). Unwinds the call stack exactly like `Thrown` (`?`
    /// propagates it through `call_static`/`call_instance` the same way),
    /// but is a distinct variant so `run_frame`'s exception-table match
    /// never intercepts it — no NL `try`/`catch` can catch a process exit.
    /// Deliberately *not* a real `std::process::exit` call at the point
    /// `system.ps.Process.exit` is dispatched: `nl_vm::run_program` runs
    /// in-process inside embedders like `nl-test-runner`, which execute many
    /// NL programs in one OS process — a literal `std::process::exit` there
    /// would kill the whole test run, not just the one program under test.
    /// `run_program` catches this variant and turns it into `RunOutcome`'s
    /// exit code instead, matching a real process exit's observable effect
    /// for that entry point without the collateral damage.
    #[error("process exit requested with code {0}")]
    Exit(i32),
}
