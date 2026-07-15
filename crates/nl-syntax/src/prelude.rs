//! Built-in exception hierarchy — specs.md § Exceptions, § Exception class
//! hierarchy. These classes are globally visible without a `use` (there is
//! no stdlib namespace to import them from yet), so every compiled program
//! must include them: callers prepend `prelude::files()` to their file list
//! before `nl_sema::check_compile`/`nl_codegen::compile_program`, and every
//! `import_map` seeds each builtin's simple name -> FQCN (identity, since
//! these files declare an empty namespace — `fqcn_of` returns the bare class
//! name when `namespace` is empty).
//!
//! Stack trace capture (vm.md § Stack trace construction) is not
//! implemented: `Exception` has no `stackTrace` field this phase.
//! Checked-exception declaration/propagation checking (E016/E017) is not
//! enforced either — see PLAN.md Phase 5.

use crate::ast::{
    Block, ClassDecl, Expr, FieldDecl, LValue, MethodDecl, MethodKind, Param, SourceFile, SourceItem, Stmt, Type,
    Visibility,
};

/// `(name, parent)` pairs describing the built-in hierarchy — specs.md §
/// Exception class hierarchy. `Exception` itself is the only root.
const HIERARCHY: &[(&str, Option<&str>)] = &[
    ("Exception", None),
    ("RuntimeException", Some("Exception")),
    ("ArithmeticException", Some("RuntimeException")),
    ("IndexOutOfBoundsException", Some("RuntimeException")),
    ("NullPointerException", Some("RuntimeException")),
    ("InvalidCastException", Some("RuntimeException")),
    ("NumberFormatException", Some("RuntimeException")),
    ("IllegalArgumentException", Some("RuntimeException")),
    ("StackOverflowException", Some("RuntimeException")),
    ("IOException", Some("Exception")),
    ("FileNotFoundException", Some("IOException")),
    ("FormatException", Some("Exception")),
    ("InterruptedException", Some("Exception")),
];

/// Qualified aliases for prelude exception classes: stdlib.md groups the
/// I/O exceptions under the `system.io` namespace (and fixtures reference
/// them that way — `catch (system.io.IOException e)` in `m7_0030`), but the
/// prelude declares them namespace-less, so both `import_map`s
/// (`nl_sema::class_table` / `nl_codegen::class_table`) seed these
/// qualified spellings as extra names for the same classes.
pub const NAMESPACED_ALIASES: &[(&str, &str)] = &[
    ("system.io.IOException", "IOException"),
    ("system.io.FileNotFoundException", "FileNotFoundException"),
    ("system.net.IOException", "IOException"),
    ("system.ps.IOException", "IOException"),
];

/// Every built-in exception class, as a namespace-less `SourceFile`.
pub fn files() -> Vec<SourceFile> {
    HIERARCHY
        .iter()
        .map(|(name, parent)| SourceFile {
            namespace: Vec::new(),
            uses: Vec::new(),
            item: SourceItem::Class(exception_class(name, *parent)),
        })
        .collect()
}

fn exception_class(name: &str, parent: Option<&str>) -> ClassDecl {
    let param = Param { name: "what".to_string(), ty: Type::StringT };
    let ctor_body: Block = match parent {
        // The root `Exception` class owns the `message` field and sets it
        // directly from its constructor argument.
        None => vec![Stmt::Expr(Expr::Assign(
            LValue::Field(Box::new(Expr::This), "message".to_string()),
            Box::new(Expr::Ident("what".to_string())),
        ))],
        // Every other class in the hierarchy just forwards to its parent.
        Some(_) => vec![Stmt::SuperCall(vec![Expr::Ident("what".to_string())])],
    };
    let ctor = MethodDecl {
        name: "<construct>".to_string(),
        kind: MethodKind::Constructor,
        visibility: Visibility::Public,
        is_static: false,
        is_const: false,
        return_type: Type::Void,
        params: vec![param],
        throws: Vec::new(),
        body: ctor_body,
    };
    let fields = if parent.is_none() {
        vec![FieldDecl {
            name: "message".to_string(),
            visibility: Visibility::Public,
            is_static: false,
            readonly: false,
            ty: Type::StringT,
            init: None,
        }]
    } else {
        Vec::new()
    };
    ClassDecl {
        name: name.to_string(),
        type_params: Vec::new(),
        extends: parent.map(str::to_string),
        implements: Vec::new(),
        fields,
        methods: vec![ctor],
    }
}
