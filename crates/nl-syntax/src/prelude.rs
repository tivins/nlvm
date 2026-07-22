//! Built-in exception hierarchy — specs.md § Exceptions, § Exception class
//! hierarchy. These classes are globally visible without a `use` (there is
//! no stdlib namespace to import them from yet), so every compiled program
//! must include them: callers prepend `prelude::files()` to their file list
//! before `nl_sema::check_compile`/`nl_codegen::compile_program`, and every
//! `import_map` seeds each builtin's simple name -> FQCN (identity, since
//! these files declare an empty namespace — `fqcn_of` returns the bare class
//! name when `namespace` is empty).
//!
//! Checked-exception declaration/propagation checking (E016/E017) is not
//! enforced either — see PLAN.md Phase 5.
//!
//! Stack trace capture (vm.md § Stack trace construction): `Exception`
//! declares `public ExecutionPoint[] stackTrace;`, but no NL source ever
//! assigns it — `nl_vm::interpreter` sets it natively (bypassing bytecode
//! entirely, the same way it builds VM-thrown exceptions like
//! `NullPointerException` without going through a constructor at all) the
//! moment `Exception.<construct>` is about to return. See
//! `nl_vm::call_stack` and `interpreter::maybe_capture_stack_trace`.

use crate::ast::{
    Arg, BinOp, Block, ClassDecl, Expr, FieldDecl, InterfaceDecl, LValue, MethodDecl, MethodKind,
    MethodSig, Param, SourceFile, SourceItem, Stmt, StmtKind, Type, TypeParam, Visibility,
};

/// Synthetic origin path stamped on every prelude `SourceFile` — these are
/// generated in Rust, not parsed from a `.nl` file, so `nlc -l`/diagnostics
/// attribute anything (in practice, nothing — the prelude is hand-verified)
/// that traces back here to this marker rather than a real path.
const PRELUDE_PATH: &str = "<prelude>";

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
    let mut files: Vec<SourceFile> = HIERARCHY
        .iter()
        .map(|(name, parent)| SourceFile {
            namespace: Vec::new(),
            uses: Vec::new(),
            item: SourceItem::Class(exception_class(name, *parent)),
            path: PRELUDE_PATH.to_string(),
        })
        .collect();
    files.push(SourceFile {
        namespace: Vec::new(),
        uses: Vec::new(),
        item: SourceItem::Interface(stringable()),
        path: PRELUDE_PATH.to_string(),
    });
    files.push(SourceFile {
        namespace: Vec::new(),
        uses: Vec::new(),
        item: SourceItem::Class(box_class()),
        path: PRELUDE_PATH.to_string(),
    });
    files.push(SourceFile {
        namespace: Vec::new(),
        uses: Vec::new(),
        item: SourceItem::Class(execution_point_class()),
        path: PRELUDE_PATH.to_string(),
    });
    files
}

/// vm.md § Stack trace construction — one entry of `Exception.stackTrace`.
/// Never constructed from NL source (`nl_vm::interpreter` builds instances
/// directly, bypassing this constructor, exactly like the VM-thrown
/// built-in exceptions bypass `Exception`'s own); declared as a real class
/// purely so `Exception.stackTrace: ExecutionPoint[]` is a type nl-sema and
/// nl-codegen can resolve like any other object array.
fn execution_point_class() -> ClassDecl {
    let fields = vec![
        FieldDecl {
            name: "file".to_string(),
            visibility: Visibility::Public,
            visibility_explicit: true,
            is_static: false,
            readonly: false,
            ty: Type::StringT,
            init: None,
        },
        FieldDecl {
            name: "line".to_string(),
            visibility: Visibility::Public,
            visibility_explicit: true,
            is_static: false,
            readonly: false,
            ty: Type::Int,
            init: None,
        },
    ];
    let params = vec![
        Param {
            name: "file".to_string(),
            ty: Type::StringT,
            is_const: false,
            default: None,
            is_ref: false,
        },
        Param {
            name: "line".to_string(),
            ty: Type::Int,
            is_const: false,
            default: None,
            is_ref: false,
        },
    ];
    let ctor_body: Block = vec![
        Stmt {
            kind: StmtKind::Expr(Expr::Assign(
                LValue::Field(Box::new(Expr::This), "file".to_string()),
                Box::new(Expr::Ident("file".to_string())),
            )),
            line: 0,
        },
        Stmt {
            kind: StmtKind::Expr(Expr::Assign(
                LValue::Field(Box::new(Expr::This), "line".to_string()),
                Box::new(Expr::Ident("line".to_string())),
            )),
            line: 0,
        },
    ];
    let ctor = MethodDecl {
        name: "<construct>".to_string(),
        kind: MethodKind::Constructor,
        visibility: Visibility::Public,
        visibility_explicit: true,
        is_static: false,
        is_const: false,
        is_abstract: false,
        is_final: false,
        is_nodiscard: false,
        return_type: Type::Void,
        params,
        throws: Vec::new(),
        body: ctor_body,
        decl_line: 0,
    };
    ClassDecl {
        name: "ExecutionPoint".to_string(),
        type_params: Vec::new(),
        extends: None,
        implements: Vec::new(),
        fields,
        methods: vec![ctor],
        is_readonly: false,
        is_abstract: false,
        is_final: false,
        decl_line: 0,
        is_enum: false,
        enum_cases: Vec::new(),
    }
}

/// vm.md § Ref parameters (boxing) — the single-field generic box a `ref`
/// parameter is passed through: the compiler `NEW`s one per call-site `ref`
/// argument, the callee reads/writes `value` in place of the parameter
/// directly, and the caller reads `value` back into its own variable after
/// the call returns. Never written by user source — `nl_syntax::monomorphize`
/// synthesizes a `Box<T>` instantiation for every concrete `T` used as a
/// `ref` parameter's type anywhere in the program (see its doc comment),
/// the same way it would for a user `new Vector<int>(...)`.
fn box_class() -> ClassDecl {
    let field = FieldDecl {
        name: "value".to_string(),
        visibility: Visibility::Public,
        visibility_explicit: true,
        is_static: false,
        readonly: false,
        ty: Type::Named("T".to_string()),
        init: None,
    };
    let param = Param {
        name: "value".to_string(),
        ty: Type::Named("T".to_string()),
        is_const: false,
        default: None,
        is_ref: false,
    };
    let ctor = MethodDecl {
        name: "<construct>".to_string(),
        kind: MethodKind::Constructor,
        visibility: Visibility::Public,
        visibility_explicit: true,
        is_static: false,
        is_const: false,
        is_abstract: false,
        is_final: false,
        is_nodiscard: false,
        return_type: Type::Void,
        params: vec![param],
        throws: Vec::new(),
        body: vec![Stmt {
            kind: StmtKind::Expr(Expr::Assign(
                LValue::Field(Box::new(Expr::This), "value".to_string()),
                Box::new(Expr::Ident("value".to_string())),
            )),
            line: 0,
        }],
        decl_line: 0,
    };
    ClassDecl {
        name: "Box".to_string(),
        type_params: vec![TypeParam {
            name: "T".to_string(),
            bound: None,
        }],
        extends: None,
        implements: Vec::new(),
        fields: vec![field],
        methods: vec![ctor],
        is_readonly: false,
        is_abstract: false,
        is_final: false,
        decl_line: 0,
        is_enum: false,
        enum_cases: Vec::new(),
    }
}

/// specs.md § Stringable interface — `public string toString() const;`. A
/// class implementing this interface must declare `toString` itself `const`
/// (compiler.md § Const methods, E044). Dynamic `toString()` dispatch for
/// string concatenation/`(string)` casts on a Stringable-implementing class
/// is not wired up (see `checker.rs`'s E008 comment / PLAN.md) — this only
/// makes the interface itself declarable and its const-correctness checked.
fn stringable() -> InterfaceDecl {
    InterfaceDecl {
        name: "Stringable".to_string(),
        methods: vec![MethodSig {
            name: "toString".to_string(),
            return_type: Type::StringT,
            params: Vec::new(),
            is_const: true,
        }],
        decl_line: 0,
    }
}

fn exception_class(name: &str, parent: Option<&str>) -> ClassDecl {
    let param = Param {
        name: "what".to_string(),
        ty: Type::StringT,
        is_const: false,
        default: None,
        is_ref: false,
    };
    let ctor_body: Block = match parent {
        // The root `Exception` class owns the `message` field and sets it
        // directly from its constructor argument.
        None => vec![Stmt {
            kind: StmtKind::Expr(Expr::Assign(
                LValue::Field(Box::new(Expr::This), "message".to_string()),
                Box::new(Expr::Ident("what".to_string())),
            )),
            line: 0,
        }],
        // Every other class in the hierarchy just forwards to its parent.
        Some(_) => vec![Stmt {
            kind: StmtKind::SuperCall(vec![Arg {
                name: None,
                is_ref: false,
                value: Expr::Ident("what".to_string()),
            }]),
            line: 0,
        }],
    };
    let ctor = MethodDecl {
        name: "<construct>".to_string(),
        kind: MethodKind::Constructor,
        visibility: Visibility::Public,
        visibility_explicit: true,
        is_static: false,
        is_const: false,
        is_abstract: false,
        is_final: false,
        is_nodiscard: false,
        return_type: Type::Void,
        params: vec![param],
        throws: Vec::new(),
        body: ctor_body,
        decl_line: 0,
    };
    let fields = if parent.is_none() {
        vec![
            FieldDecl {
                name: "message".to_string(),
                visibility: Visibility::Public,
                visibility_explicit: true,
                is_static: false,
                readonly: false,
                ty: Type::StringT,
                init: None,
            },
            // vm.md § Stack trace construction — never assigned by this
            // class's own `<construct>` body (see module doc comment); the
            // VM sets it natively right before that constructor returns.
            FieldDecl {
                name: "stackTrace".to_string(),
                visibility: Visibility::Public,
                visibility_explicit: true,
                is_static: false,
                readonly: false,
                ty: Type::Array(Box::new(Type::Named("ExecutionPoint".to_string()))),
                init: None,
            },
        ]
    } else {
        Vec::new()
    };
    // specs.md § Exception class hierarchy: `printStackTrace()` is declared
    // only on the root `Exception` — every subclass inherits it via the
    // ordinary virtual-dispatch/`extends` mechanism (same as `describe()` in
    // a user-defined hierarchy), so there's no need to redeclare it on each
    // one.
    let methods = if parent.is_none() {
        vec![ctor, print_stack_trace_method()]
    } else {
        vec![ctor]
    };
    ClassDecl {
        name: name.to_string(),
        type_params: Vec::new(),
        extends: parent.map(str::to_string),
        implements: Vec::new(),
        fields,
        methods,
        // specs.md § Exception class hierarchy declares every one of these
        // `class readonly ExceptionName { ... }` — safe to mark since the
        // only place any of them ever assigns a field is their own
        // `<construct>` (`this.message = ...` on the root, `super(...)`
        // everywhere else), which compiler.md's readonly rule always allows.
        is_readonly: true,
        is_abstract: false,
        is_final: false,
        decl_line: 0,
        is_enum: false,
        enum_cases: Vec::new(),
    }
}

/// specs.md § Exception class hierarchy, `printStackTrace()`: writes
/// `message` to `system.Err`, followed by one `"    at " + file + ":" +
/// line` line per `stackTrace` frame, in capture order (throw site first).
/// Built as AST here (rather than parsed `.nl` source, like the rest of
/// this file) so it type-checks and compiles through the ordinary
/// `nl-sema`/`nl-codegen` pipeline exactly like a hand-written override
/// would — no new native VM dispatch is needed, since `system.Err.println`
/// is already native (see `nl_vm::native`).
///
/// **Known limitation** (specs.md, same section): without a reflection API,
/// this cannot prefix the output with the exception's runtime class name
/// the way Java's `Throwable.printStackTrace()` does — only `message` and
/// the frame list are available.
fn print_stack_trace_method() -> MethodDecl {
    fn println_stderr(arg: Expr) -> Stmt {
        Stmt {
            kind: StmtKind::Expr(Expr::MethodCall(
                Box::new(Expr::FieldAccess(
                    Box::new(Expr::Ident("system".to_string())),
                    "Err".to_string(),
                )),
                "println".to_string(),
                vec![Arg {
                    name: None,
                    is_ref: false,
                    value: arg,
                }],
            )),
            line: 0,
        }
    }

    let message_line = println_stderr(Expr::FieldAccess(
        Box::new(Expr::This),
        "message".to_string(),
    ));

    // `"    at " + point.file + ":" + point.line` — left-associative, exactly
    // as written in specs.md.
    let frame_line = Expr::Binary(
        BinOp::Add,
        Box::new(Expr::Binary(
            BinOp::Add,
            Box::new(Expr::Binary(
                BinOp::Add,
                Box::new(Expr::StringLit("    at ".to_string())),
                Box::new(Expr::FieldAccess(
                    Box::new(Expr::Ident("point".to_string())),
                    "file".to_string(),
                )),
            )),
            Box::new(Expr::StringLit(":".to_string())),
        )),
        Box::new(Expr::FieldAccess(
            Box::new(Expr::Ident("point".to_string())),
            "line".to_string(),
        )),
    );

    let frames_loop = Stmt {
        kind: StmtKind::ForEach {
            ty: None,
            var: "point".to_string(),
            iterable: Expr::FieldAccess(Box::new(Expr::This), "stackTrace".to_string()),
            body: vec![println_stderr(frame_line)],
        },
        line: 0,
    };

    MethodDecl {
        name: "printStackTrace".to_string(),
        kind: MethodKind::Normal,
        visibility: Visibility::Public,
        visibility_explicit: true,
        is_static: false,
        is_const: false,
        is_abstract: false,
        is_final: false,
        is_nodiscard: false,
        return_type: Type::Void,
        params: Vec::new(),
        throws: Vec::new(),
        body: vec![message_line, frames_loop],
        decl_line: 0,
    }
}
