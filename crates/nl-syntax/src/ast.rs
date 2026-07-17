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
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    /// compiler.md § Const parameters — E012: cannot be reassigned/mutated in
    /// the method body, and (for object types) only `const` methods may be
    /// called on it.
    pub is_const: bool,
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
}

pub type Block = Vec<Stmt>;

/// Assignable expression forms — see specs.md § Assignment operators.
#[derive(Debug, Clone, PartialEq)]
pub enum LValue {
    Local(String),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
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
    ThisCall(Vec<Expr>),
    /// `super(args);` constructor delegation to the direct superclass — like
    /// `ThisCall`, must be the first statement of a constructor body.
    SuperCall(Vec<Expr>),
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
    Call(String, Vec<Expr>),
    /// `new ClassName(args)` or `new ClassName<TypeArgs>(args)` — see
    /// `nl_syntax::monomorphize`, which rewrites the latter into the
    /// former (against a mangled class name) before this ever reaches
    /// `nl-sema`/`nl-codegen`.
    New(String, Vec<Type>, Vec<Expr>),
    /// `new T[size]` — fixed-size single-dimension array creation.
    NewArray(Box<Type>, Box<Expr>),
    /// `new T[]{ e0, e1, ... }` — initializer-list array creation, size is
    /// the element count (specs.md § Arrays, "Initializer list").
    NewArrayInit(Box<Type>, Vec<Expr>),
    /// `target.field`.
    FieldAccess(Box<Expr>, String),
    /// `target.method(args)`.
    MethodCall(Box<Expr>, String, Vec<Expr>),
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
    /// 10 (below `||`, above `??`/`?:` elvis — the latter two are not
    /// implemented yet).
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
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
