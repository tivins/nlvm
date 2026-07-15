#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SemaError {
    #[error("E027 — No 'main' method found")]
    NoMainMethod,
    #[error("E028 — Multiple 'main' methods found")]
    MultipleMainMethods,
    #[error("E029 — 'main' method has incorrect signature (expected: public static int main(string[]))")]
    BadMainSignature,
}

impl SemaError {
    /// The `E###`/`W###` code, as used by `expected_compile_error` in test YAML files.
    pub fn code(&self) -> &'static str {
        match self {
            SemaError::NoMainMethod => "E027",
            SemaError::MultipleMainMethods => "E028",
            SemaError::BadMainSignature => "E029",
        }
    }
}
