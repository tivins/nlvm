#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SemaError {
    #[error("E001 — Variable '{0}' may not have been initialized")]
    NotDefinitelyAssigned(String),
    #[error("E003 — Cannot assign 'null' to type '{0}' (type does not include null)")]
    NullToNonNullable(String),
    #[error("E004 — Type '{0}' is not assignable to '{1}'")]
    NotAssignable(String, String),
    #[error("E005 — Cannot use 'auto' without an initializer")]
    AutoWithoutInitializer,
    #[error("E008 — Cannot concatenate 'string' with type '{0}' (type does not implement Stringable)")]
    BadConcatenation(String),
    #[error("E009 — Operator '{0}' is not defined for types '{1}' and '{2}'")]
    BadBinaryOperator(String, String, String),
    #[error("E009 — Operator '{0}' is not defined for type '{1}'")]
    BadUnaryOperator(String, String),
    #[error("E041 — Duplicate method '{0}' with identical signature in class '{1}'")]
    DuplicateMethod(String, String),
    #[error("E042 — Duplicate class definition '{0}'")]
    DuplicateClass(String),
    #[error("E045 — 'this(...)' delegation call must be the first statement of the constructor")]
    ThisCallNotFirst,
    #[error("E046 — Constructor delegation cycle in class '{0}'")]
    DelegationCycle(String),
    #[error("E047 — Match expression is not exhaustive (missing '{0}')")]
    MatchNotExhaustive(String),
    #[error("E048 — Unreachable catch clause: '{0}' is already caught by earlier clause '{1}'")]
    UnreachableCatch(String, String),

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
            SemaError::NotDefinitelyAssigned(_) => "E001",
            SemaError::NullToNonNullable(_) => "E003",
            SemaError::NotAssignable(_, _) => "E004",
            SemaError::AutoWithoutInitializer => "E005",
            SemaError::BadConcatenation(_) => "E008",
            SemaError::BadBinaryOperator(_, _, _) => "E009",
            SemaError::BadUnaryOperator(_, _) => "E009",
            SemaError::DuplicateMethod(_, _) => "E041",
            SemaError::DuplicateClass(_) => "E042",
            SemaError::ThisCallNotFirst => "E045",
            SemaError::DelegationCycle(_) => "E046",
            SemaError::MatchNotExhaustive(_) => "E047",
            SemaError::UnreachableCatch(_, _) => "E048",
            SemaError::NoMainMethod => "E027",
            SemaError::MultipleMainMethods => "E028",
            SemaError::BadMainSignature => "E029",
        }
    }
}
