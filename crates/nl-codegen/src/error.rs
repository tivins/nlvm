#[derive(Debug, thiserror::Error)]
pub enum CodegenError {
    #[error("unsupported construct: {0}")]
    Unsupported(String),
}
