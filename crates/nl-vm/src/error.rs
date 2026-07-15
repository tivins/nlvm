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
    #[error("ArithmeticException: division by zero")]
    DivisionByZero,
    #[error("NullPointerException")]
    NullPointer,
    #[error("IndexOutOfBoundsException: index {index}, length {length}")]
    IndexOutOfBounds { index: i64, length: usize },
    #[error("unsupported opcode in this milestone: {0}")]
    Unsupported(String),
    #[error("malformed bytecode: {0}")]
    Malformed(&'static str),
}
