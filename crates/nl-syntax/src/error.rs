#[derive(Debug, thiserror::Error)]
pub enum SyntaxError {
    #[error("lex error at line {1}: {0}")]
    Lex(String, u32),
    #[error("parse error at line {1}: {0}")]
    Parse(String, u32),
}
