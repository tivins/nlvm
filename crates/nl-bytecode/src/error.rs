#[derive(Debug, thiserror::Error)]
pub enum BytecodeError {
    #[error("bad magic number: {0:#010x}")]
    BadMagic(u32),
    #[error("unexpected end of module bytes")]
    UnexpectedEof,
    #[error("unknown constant pool tag: {0}")]
    UnknownConstantTag(u8),
    #[error("unknown hash algorithm: {0}")]
    UnknownHashAlgo(u8),
    #[error("integrity hash mismatch — module is corrupted or tampered with")]
    HashMismatch,
    #[error("malformed module: {0}")]
    Malformed(&'static str),
}
