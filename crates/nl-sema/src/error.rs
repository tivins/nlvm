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
    #[error("E007 — Cannot cast '{0}' to '{1}'")]
    BadCast(String, String),
    #[error(
        "E008 — Cannot concatenate 'string' with type '{0}' (type does not implement Stringable)"
    )]
    BadConcatenation(String),
    #[error("E009 — Operator '{0}' is not defined for types '{1}' and '{2}'")]
    BadBinaryOperator(String, String, String),
    #[error("E009 — Operator '{0}' is not defined for type '{1}'")]
    BadUnaryOperator(String, String),
    #[error("E041 — Duplicate method '{0}' with identical signature in class '{1}'")]
    DuplicateMethod(String, String),
    #[error("E041 — Method '{0}' is inherited from both '{1}' and '{2}' with different return types (diamond merge in '{3}')")]
    DiamondInterfaceConflict(String, String, String, String),
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
    #[error("E015 — Unhandled checked exception '{0}' — must be caught or declared in 'throws'")]
    UnhandledCheckedException(String),
    #[error("E016 — Overriding method '{0}' does not declare exception '{1}' from parent method")]
    MissingThrowsInOverride(String, String),
    #[error("E017 — Overriding method '{0}' declares exception '{1}' not thrown by parent method")]
    ExtraThrowsInOverride(String, String),

    #[error("E027 — No 'main' method found")]
    NoMainMethod,
    #[error("E028 — Multiple 'main' methods found")]
    MultipleMainMethods,
    #[error(
        "E029 — 'main' method has incorrect signature (expected: public static int main(string[]))"
    )]
    BadMainSignature,
    #[error("E040 — Cannot use '{0}' in a static context")]
    StaticContextMisuse(String),
    #[error("E043 — Import creates duplicate symbol '{0}'")]
    DuplicateImportSymbol(String),
    #[error(
        "E031 — Cannot create array of non-nullable type '{0}' with fixed size (no default value)"
    )]
    NonNullableArrayFixedSize(String),
    #[error("E018 — Member '{0}' is not accessible from '{1}' (visibility: {2})")]
    MemberNotAccessible(String, String, String),
    #[error("E019 — Missing visibility modifier on member '{0}'")]
    MissingVisibilityModifier(String),
    #[error("E002 — Property '{0}' of non-nullable type '{1}' is not initialized in constructor")]
    PropertyNotInitialized(String, String),
    #[error("E010 — Cannot modify property '{0}' in a const method")]
    ConstMethodPropertyModification(String),
    #[error("E011 — Cannot call non-const method '{0}' in a const method")]
    ConstMethodNonConstCall(String),
    #[error("E044 — Method '{0}' implementing interface '{1}' must be declared const")]
    MethodMustBeConst(String, String),
    #[error("E012 — Cannot modify const variable '{0}'")]
    ConstModification(String),
    #[error("E039 — Cannot modify loop variable '{0}' — implicitly const when iterating over read-only collection")]
    ConstLoopVariableModification(String),
    #[error("E037 — Type '{0}' does not satisfy bound '{1}' (required by template '{2}')")]
    TemplateBoundNotSatisfied(String, String, String),
    #[error("E006 — Type '{0}' does not support operator '{1}' (required by template '{2}')")]
    TemplateOperatorUnsupported(String, String, String),
    #[error("E013 — Cannot modify property '{0}' of readonly class '{1}'")]
    ReadonlyClassModification(String, String),
    #[error("E014 — Cannot modify readonly property '{0}'")]
    ReadonlyPropertyModification(String),
    #[error("E032 — Cannot instantiate abstract class '{0}'")]
    InstantiateAbstractClass(String),
    #[error(
        "E033 — Class '{0}' must be declared abstract (has unimplemented abstract method '{1}')"
    )]
    ClassMustBeAbstract(String, String),
    #[error("E034 — Abstract method '{0}' cannot have a body")]
    AbstractMethodHasBody(String),
    #[error("E035 — Cannot extend final class '{0}'")]
    ExtendFinalClass(String),
    #[error("E036 — Cannot override final method '{0}'")]
    OverrideFinalMethod(String),
    #[error("E049 — Conflicting modifiers 'abstract' and 'final' on '{0}'")]
    ConflictingModifiers(String),
    #[error("E038 — Non-first dimension size omitted in middle position in '{0}'")]
    NonContiguousArrayDimensionOmission(String),
    #[error("E023 — Required parameter '{0}' not provided")]
    RequiredParamNotProvided(String),
    #[error("E024 — Positional argument after named argument")]
    PositionalArgAfterNamed,
    #[error("E025 — Parameter '{0}' provided both positionally and by name")]
    ParamProvidedTwice(String),
    #[error("E026 — Default value for parameter '{0}' must be a compile-time constant")]
    DefaultNotConstant(String),
    #[error("E020 — Argument for 'ref' parameter '{0}' must be a variable")]
    RefArgNotVariable(String),
    #[error("E021 — Missing 'ref' keyword at call site for parameter '{0}'")]
    MissingRefKeyword(String),
    #[error("E022 — Optional parameters cannot be declared 'ref'")]
    OptionalCannotBeRef,
}

impl SemaError {
    /// The `E###`/`W###` code, as used by `expected_compile_error` in test YAML files.
    pub fn code(&self) -> &'static str {
        match self {
            SemaError::NotDefinitelyAssigned(_) => "E001",
            SemaError::NullToNonNullable(_) => "E003",
            SemaError::NotAssignable(_, _) => "E004",
            SemaError::AutoWithoutInitializer => "E005",
            SemaError::BadCast(_, _) => "E007",
            SemaError::BadConcatenation(_) => "E008",
            SemaError::BadBinaryOperator(_, _, _) => "E009",
            SemaError::BadUnaryOperator(_, _) => "E009",
            SemaError::DuplicateMethod(_, _) => "E041",
            SemaError::DiamondInterfaceConflict(_, _, _, _) => "E041",
            SemaError::DuplicateClass(_) => "E042",
            SemaError::ThisCallNotFirst => "E045",
            SemaError::DelegationCycle(_) => "E046",
            SemaError::MatchNotExhaustive(_) => "E047",
            SemaError::UnreachableCatch(_, _) => "E048",
            SemaError::UnhandledCheckedException(_) => "E015",
            SemaError::MissingThrowsInOverride(_, _) => "E016",
            SemaError::ExtraThrowsInOverride(_, _) => "E017",
            SemaError::NoMainMethod => "E027",
            SemaError::MultipleMainMethods => "E028",
            SemaError::BadMainSignature => "E029",
            SemaError::StaticContextMisuse(_) => "E040",
            SemaError::DuplicateImportSymbol(_) => "E043",
            SemaError::NonNullableArrayFixedSize(_) => "E031",
            SemaError::MemberNotAccessible(_, _, _) => "E018",
            SemaError::MissingVisibilityModifier(_) => "E019",
            SemaError::PropertyNotInitialized(_, _) => "E002",
            SemaError::ConstMethodPropertyModification(_) => "E010",
            SemaError::ConstMethodNonConstCall(_) => "E011",
            SemaError::MethodMustBeConst(_, _) => "E044",
            SemaError::ConstModification(_) => "E012",
            SemaError::ConstLoopVariableModification(_) => "E039",
            SemaError::TemplateBoundNotSatisfied(_, _, _) => "E037",
            SemaError::TemplateOperatorUnsupported(_, _, _) => "E006",
            SemaError::ReadonlyClassModification(_, _) => "E013",
            SemaError::ReadonlyPropertyModification(_) => "E014",
            SemaError::InstantiateAbstractClass(_) => "E032",
            SemaError::ClassMustBeAbstract(_, _) => "E033",
            SemaError::AbstractMethodHasBody(_) => "E034",
            SemaError::ExtendFinalClass(_) => "E035",
            SemaError::OverrideFinalMethod(_) => "E036",
            SemaError::ConflictingModifiers(_) => "E049",
            SemaError::NonContiguousArrayDimensionOmission(_) => "E038",
            SemaError::RequiredParamNotProvided(_) => "E023",
            SemaError::PositionalArgAfterNamed => "E024",
            SemaError::ParamProvidedTwice(_) => "E025",
            SemaError::DefaultNotConstant(_) => "E026",
            SemaError::RefArgNotVariable(_) => "E020",
            SemaError::MissingRefKeyword(_) => "E021",
            SemaError::OptionalCannotBeRef => "E022",
        }
    }
}

/// A `SemaError` with the source location it was raised at — `nlc -l`/other
/// diagnostics consumers use this instead of the bare `SemaError` to report
/// `file:line: E0XX — message`. Line is statement granularity inside a
/// method body, declaration granularity (class/method) for structural
/// checks that have no single enclosing statement — see
/// `checker::check_source_file`, the only place this is constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocatedError {
    pub file: String,
    pub line: u32,
    pub error: SemaError,
}

impl LocatedError {
    pub fn code(&self) -> &'static str {
        self.error.code()
    }
}

impl std::fmt::Display for LocatedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.file.is_empty() {
            write!(f, "{}", self.error)
        } else {
            write!(f, "{}:{}: {}", self.file, self.line, self.error)
        }
    }
}

impl std::error::Error for LocatedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// A non-fatal diagnostic — compiler.md § Warnings: reported alongside a
/// successful compilation, never turns it into a `LocatedError`. Only W001
/// exists so far (specs.md § Nodiscard).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SemaWarning {
    #[error("W001 — Return value of nodiscard method '{0}' is discarded")]
    NodiscardDiscarded(String),
}

impl SemaWarning {
    pub fn code(&self) -> &'static str {
        match self {
            SemaWarning::NodiscardDiscarded(_) => "W001",
        }
    }
}

/// A `SemaWarning` with the source location it was raised at — see
/// `LocatedError`, same idea for warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocatedWarning {
    pub file: String,
    pub line: u32,
    pub warning: SemaWarning,
}

impl LocatedWarning {
    pub fn code(&self) -> &'static str {
        self.warning.code()
    }
}

impl std::fmt::Display for LocatedWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.file.is_empty() {
            write!(f, "{}", self.warning)
        } else {
            write!(f, "{}:{}: {}", self.file, self.line, self.warning)
        }
    }
}
