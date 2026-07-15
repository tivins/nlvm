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
}
