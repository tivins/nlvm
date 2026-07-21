use serde::Deserialize;

/// YAML front matter — see nlvm-specs/docs/tests.md § Header keys.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Header {
    #[allow(dead_code)]
    pub title: Option<String>,
    pub file_separator: Option<String>,
    pub expected_exit_code: Option<i32>,
    pub expected_stdout: Option<String>,
    pub expected_stderr: Option<String>,
    pub compile_only: Option<bool>,
    pub expected_compile_error: Option<String>,
    /// nlvm-internal extension, not part of nlvm-specs/docs/tests.md: expects
    /// `nl_syntax::parse_source_file` itself to fail (e.g. a second top-level
    /// declaration in one file), as opposed to `expected_compile_error` which
    /// covers `nl-sema`'s E-code diagnostics on an AST that parsed fine.
    pub expected_parse_error: Option<bool>,
    /// nlvm-internal extension, not part of nlvm-specs/docs/tests.md (same
    /// spirit as `expected_parse_error`): expects `nl_sema::check_compile`
    /// to succeed but report the given warning code (e.g. `W001`) somewhere
    /// among its diagnostics. Compilation must still succeed — a warning
    /// never fails the build (compiler.md § Warnings).
    pub expected_warning: Option<String>,
    pub expected_class: Option<String>,
    pub expected_methods: Option<Vec<String>>,
    pub expected_fields: Option<Vec<serde_yaml::Value>>,
    pub expected_constant_pool_contains: Option<Vec<String>>,
}

impl Header {
    pub fn file_separator_or_default(&self) -> &str {
        self.file_separator.as_deref().unwrap_or("#NLFILE")
    }

    pub fn is_compile_only(&self) -> bool {
        self.compile_only.unwrap_or(false)
    }
}
