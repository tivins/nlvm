//! AST — covers the subset of nlvm-specs/docs/specs.md needed so far
//! (namespace, classes with fields/constructors/instance methods, interfaces,
//! arithmetic/logical expressions, objects, arrays, `return`). Extended
//! incrementally as later milestones are implemented.

#[derive(Debug, Clone, PartialEq)]
pub struct SourceFile {
    pub namespace: Vec<String>,
    /// Fully-qualified names brought into scope by `use ns.path.Name [as
    /// Alias];` clauses, in source order.
    pub uses: Vec<UseDecl>,
    pub item: SourceItem,
    /// Origin path for diagnostics (`nlc -l`/linter output, `file:line: ...`)
    /// — the path passed to `parse_source_file`, or a synthetic marker like
    /// `"<prelude>"` for built-in files (see `nl_syntax::prelude`).
    pub path: String,
}

/// A single `use` clause. `path` is the dotted FQCN (e.g.
/// `"test.class.ClassTest"`); `alias` is the optional `as Alias` local name
/// (specs.md § Imports, "Using aliases") — when absent, importers bind the
/// last segment of `path` as usual.
#[derive(Debug, Clone, PartialEq)]
pub struct UseDecl {
    pub path: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceItem {
    Class(ClassDecl),
    Interface(InterfaceDecl),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Protected,
    Private,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassDecl {
    pub name: String,
    /// `template <type T, type U, ...>` parameters, empty for an ordinary
    /// (non-template) class — specs.md § Template class. `Self`/`type`
    /// contextual sugar inside a template body is not supported, bodies must
    /// spell out the type parameter's own name instead.
    pub type_params: Vec<TypeParam>,
    pub extends: Option<String>,
    pub implements: Vec<String>,
    pub fields: Vec<FieldDecl>,
    pub methods: Vec<MethodDecl>,
    /// `class readonly Name` — specs.md § Readonly. After construction, no
    /// property of any instance can be modified outside `construct`
    /// (compiler.md § Readonly classes and properties, E013).
    pub is_readonly: bool,
    /// `abstract class Name` — specs.md § Abstract classes and methods.
    /// Cannot be instantiated (E032); a class that declares or inherits an
    /// unimplemented abstract method must be `abstract` (E033).
    pub is_abstract: bool,
    /// `final class Name` — specs.md § Final classes and methods. Cannot be
    /// `extends`-ed (E035). Mutually exclusive with `is_abstract` (E049).
    pub is_final: bool,
    /// Source line of the `class`/`template` keyword — used to locate
    /// declaration-granularity diagnostics (duplicate class, abstract/final
    /// consistency, etc.) that have no more specific statement to point at.
    pub decl_line: u32,
    /// `enum Name [: int|string] { ... }` — specs.md § Enums, vm.md § Enum
    /// representation. The parser (`parse_enum_decl`) desugars an enum
    /// declaration straight into an ordinary `ClassDecl`: each case becomes a
    /// `static readonly` field (see `enum_cases`) of the backing type
    /// (`int` for a basic enum with no backing type, or the declared `int`/
    /// `string` backing), and `from()`/`tryFrom()` are synthesized as
    /// ordinary static methods. This flag only controls the `ENUM` class
    /// flag bit emitted by `nl-codegen` — everything else about an enum
    /// reuses the plain class pipeline.
    pub is_enum: bool,
    /// Case names in declaration order, empty for a non-enum class. Each
    /// name is also present in `fields` (as the first `enum_cases.len()`
    /// entries, static+readonly) — kept here too so `nl-sema`/`nl-codegen`
    /// can tell a case constant (whose *static type* is the enum itself,
    /// e.g. `Status.OK : Status`) apart from an ordinary static field
    /// (whose type is its own declared type) without re-deriving the case
    /// list from field order.
    pub enum_cases: Vec<String>,
}

/// One `type T [extends Bound]` template parameter — specs.md § Bounded type
/// parameters. `bound` is checked at instantiation time (compiler.md §
/// Template instantiation, E037) against the concrete type argument.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeParam {
    pub name: String,
    pub bound: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    pub name: String,
    pub visibility: Visibility,
    /// Whether `public`/`private`/`protected` was written explicitly in the
    /// source, as opposed to defaulting to `Visibility::Public` on omission
    /// — compiler.md § Visibility enforcement requires every member to carry
    /// an explicit modifier (E019).
    pub visibility_explicit: bool,
    pub is_static: bool,
    pub readonly: bool,
    pub ty: Type,
    pub init: Option<Expr>,
}

/// Interface method declarations have a signature only — no body.
#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceDecl {
    pub name: String,
    pub methods: Vec<MethodSig>,
    /// See `ClassDecl::decl_line`.
    pub decl_line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodSig {
    pub name: String,
    pub return_type: Type,
    pub params: Vec<Param>,
    /// `const` after the parameter list — compiler.md § Const methods: a
    /// class implementing this interface method must declare it `const` too
    /// (E044).
    pub is_const: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    Normal,
    Constructor,
    Destructor,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodDecl {
    pub name: String,
    pub kind: MethodKind,
    pub visibility: Visibility,
    /// See `FieldDecl::visibility_explicit` — E019.
    pub visibility_explicit: bool,
    pub is_static: bool,
    pub is_const: bool,
    /// specs.md § Abstract classes and methods — no body (`;` instead of
    /// `{ ... }`); `body` is always empty when this is `true`. Must be
    /// implemented (overridden with a real body) by every concrete
    /// subclass (compiler.md § Abstract classes and methods, E033).
    pub is_abstract: bool,
    /// `final` method modifier — specs.md § Final classes and methods.
    /// Cannot be overridden by a subclass (compiler.md, E036). Mutually
    /// exclusive with `is_abstract` (E049).
    pub is_final: bool,
    pub return_type: Type,
    pub params: Vec<Param>,
    /// `throws T1, T2, ...` — parsed and carried into bytecode metadata, but
    /// not yet statically enforced (checked-exception propagation, E016/E017,
    /// is future work; see PLAN.md Phase 5).
    pub throws: Vec<String>,
    pub body: Block,
    /// See `ClassDecl::decl_line`, same idea at method granularity.
    pub decl_line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    /// compiler.md § Const parameters — E012: cannot be reassigned/mutated in
    /// the method body, and (for object types) only `const` methods may be
    /// called on it.
    pub is_const: bool,
    /// `T name = expr` — specs.md § Optional parameters. Only trailing
    /// parameters may have one; the expression must be a compile-time
    /// constant (E026), both checked by `nl-sema`.
    pub default: Option<Expr>,
    /// `ref T name` — specs.md § Ref parameters. The method receives a true
    /// reference to the caller's variable (vm.md § Ref parameters
    /// (boxing)); the call site must use the `ref` keyword too (E021), the
    /// argument must be a variable (E020), and optional parameters can't be
    /// `ref` (E022). Independent of `is_const` (`const ref` = read-only
    /// reference).
    pub is_ref: bool,
}

/// One call-site argument — `expr` (positional) or `name: expr` (named,
/// specs.md § Named parameters). The parser accepts any order; `nl-sema`
/// does the binding against the callee's signature and validation
/// (E023-E026).
#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: Option<String>,
    /// `ref expr` — compiler.md § Ref parameter rules (E020/E021).
    pub is_ref: bool,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int,
    Float,
    Bool,
    Byte,
    StringT,
    Void,
    Array(Box<Type>),
    Named(String),
    /// The `null` member of a union type (e.g. the `null` in `string|null`).
    /// Only meaningful as a member of `Union`, or as the static type of the
    /// `null` literal expression itself.
    NullT,
    /// `Type1|Type2|...` — see specs.md § Union types and explicit nullable.
    Union(Vec<Type>),
    /// `Name<Arg1, Arg2, ...>` — a reference to a template class with
    /// concrete type arguments (specs.md § Template class), e.g.
    /// `Vector<int>`. Resolved away by `nl_syntax::monomorphize` before
    /// `nl-sema`/`nl-codegen` ever see it — they only ever encounter the
    /// monomorphized class's plain `Type::Named("ns.Vector<int>")`.
    Generic(String, Vec<Type>),
    /// `(Type, Type, ...) => ReturnType [throws Name, ...]` — specs.md §
    /// Function type assignment. `throws` is parsed for grammar
    /// completeness but not enforced (same leniency already established for
    /// `Expr::Closure`'s own `throws` field — see its doc comment).
    /// Structural: two `Function`s are the same type iff their params and
    /// return type match, regardless of which closure literal (if any)
    /// produced them — unlike a closure literal's own codegen-side type,
    /// which is tied to one synthetic class (see `nl-codegen`'s
    /// `ExprTy::Closure` doc comment).
    Function {
        params: Vec<Type>,
        return_type: Box<Type>,
        throws: Vec<String>,
    },
}

pub type Block = Vec<Stmt>;

/// Assignable expression forms — see specs.md § Assignment operators.
#[derive(Debug, Clone, PartialEq)]
pub enum LValue {
    Local(String),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
}

/// A statement plus the source line it starts on — used for
/// declaration/statement-granularity diagnostics (see `nl_sema::LocatedError`).
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    Return(Option<Expr>),
    Expr(Expr),
    VarDecl {
        ty: Option<Type>,
        name: String,
        init: Option<Expr>,
        /// compiler.md § Const local variables — E012: same rule as
        /// `Param::is_const`, cannot be reassigned/mutated after its initial
        /// assignment.
        is_const: bool,
    },
    If {
        cond: Expr,
        then_branch: Block,
        else_branch: Option<Block>,
    },
    While {
        cond: Expr,
        body: Block,
    },
    For {
        init: Vec<Stmt>,
        cond: Option<Expr>,
        step: Vec<Expr>,
        body: Block,
    },
    /// `for ([const] auto item : collection)` / `for ([const] T item :
    /// collection)` — specs.md § Loops. `ty` is `None` for `auto` (the
    /// element type is deduced from the collection). `const` on the loop
    /// variable is parsed and discarded, like `const` everywhere else in
    /// this implementation (const-correctness is out of scope — PLAN.md).
    ForEach {
        ty: Option<Type>,
        var: String,
        iterable: Expr,
        body: Block,
    },
    Break,
    Continue,
    Block(Block),
    /// `this(args);` constructor delegation — must be the first statement of
    /// a constructor body (compiler.md § Constructor delegation, E045).
    ThisCall(Vec<Arg>),
    /// `super(args);` constructor delegation to the direct superclass — like
    /// `ThisCall`, must be the first statement of a constructor body.
    SuperCall(Vec<Arg>),
    Throw(Expr),
    Try {
        body: Block,
        catches: Vec<CatchClause>,
        finally: Option<Block>,
    },
}

/// One `catch (Type name) { ... }` clause of a `Stmt::Try` — specs.md §
/// Exception handling.
#[derive(Debug, Clone, PartialEq)]
pub struct CatchClause {
    pub ty: String,
    pub var: String,
    pub body: Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Cmp3,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    IntLit(i64),
    FloatLit(f64),
    BoolLit(bool),
    StringLit(String),
    NullLit,
    This,
    /// `super` — only valid as the receiver of a field/method access
    /// (`super.field`, `super.method(...)`); `super(...)` constructor
    /// delegation is `Stmt::SuperCall` instead.
    Super,
    Ident(String),
    Assign(LValue, Box<Expr>),
    Call(String, Vec<Arg>),
    /// `new ClassName(args)` or `new ClassName<TypeArgs>(args)` — see
    /// `nl_syntax::monomorphize`, which rewrites the latter into the
    /// former (against a mangled class name) before this ever reaches
    /// `nl-sema`/`nl-codegen`.
    New(String, Vec<Type>, Vec<Arg>),
    /// `new T[n1][n2]...[nk]` — fixed-size array creation, one entry per
    /// bracket pair (`None` for an omitted size, e.g. the `[]` in
    /// `new int[3][]`). compiler.md § Multidimensional array creation:
    /// omitted sizes must form a contiguous suffix from the right (checked
    /// by `nl-sema`, E038) — everything from the first `None` onward stays
    /// unallocated (`null`).
    NewArray(Box<Type>, Vec<Option<Expr>>),
    /// `new T[]{ e0, e1, ... }` — initializer-list array creation, size is
    /// the element count (specs.md § Arrays, "Initializer list").
    NewArrayInit(Box<Type>, Vec<Expr>),
    /// `target.field`.
    FieldAccess(Box<Expr>, String),
    /// `target.method(args)`.
    MethodCall(Box<Expr>, String, Vec<Arg>),
    /// `array[index]`.
    Index(Box<Expr>, Box<Expr>),
    /// `expr instanceof TypeName`.
    InstanceOf(Box<Expr>, String),
    /// `(T) expr` — specs.md § Type conversions and casting. Validity
    /// (numeric widening/narrowing, `string`, class up/downcast) is
    /// nl-sema's job (E007); `Type` carries the full target (array/union
    /// members included), unlike `InstanceOf`'s bare name, since a cast
    /// target can be any type, not just a class/interface.
    Cast(Box<Type>, Box<Expr>),
    PostIncr(String),
    PostDecr(String),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `match(subject) { pattern: value, ..., default: value }` — specs.md §
    /// Switch/Match. Exhaustiveness (E047) is nl-sema's job; a `None`
    /// pattern is the `default` arm and must be last.
    Match(Box<Expr>, Vec<MatchArm>),
    /// `cond ? then : else` — specs.md § Ternary operator. Precedence level
    /// 10 (below `||`, above `??`/`?:` elvis).
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `a ?? b` — specs.md § Nullish coalescing operator. Precedence level
    /// 11, left-associative, looser than ternary. Only `null` (not `false`
    /// or `0`) triggers `b`.
    Coalesce(Box<Expr>, Box<Expr>),
    /// `a ?: b` — specs.md § Elvis operator. Same precedence as `??`; `b` is
    /// used when `a` is falsy (`null`, `false`, or `0`).
    Elvis(Box<Expr>, Box<Expr>),
    /// `(params) => body` — specs.md § Anonymous Functions. `return_type`
    /// is `None` when deduced from the body (only an explicit *primitive*
    /// return type is parseable today — see `nl_syntax::parser`'s
    /// `parse_closure` for why a `Named` return type is out of scope: it's
    /// ambiguous with the start of an expression body). Captured variables
    /// are copied by value at the closure's creation point (`nl-codegen`);
    /// the spec's by-reference/boxed capture (so a closure can observe or
    /// make visible mutations to a captured variable) is not implemented —
    /// see PLAN.md Phase 5.
    Closure {
        params: Vec<Param>,
        return_type: Option<Type>,
        throws: Vec<String>,
        body: ClosureBody,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClosureBody {
    Block(Block),
    Expr(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Option<Expr>,
    pub value: Expr,
}
