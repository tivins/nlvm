//! AST — covers the subset of nlvm-specs/docs/specs.md needed so far
//! (namespace, classes with fields/constructors/instance methods, interfaces,
//! arithmetic/logical expressions, objects, arrays, `return`). Extended
//! incrementally as later milestones are implemented.

#[derive(Debug, Clone, PartialEq)]
pub struct SourceFile {
    pub namespace: Vec<String>,
    /// Fully-qualified names brought into scope by `use ns.path.Name;`
    /// clauses (e.g. `"test.class.ClassTest"`), in source order.
    pub uses: Vec<String>,
    pub item: SourceItem,
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
    pub extends: Option<String>,
    pub implements: Vec<String>,
    pub fields: Vec<FieldDecl>,
    pub methods: Vec<MethodDecl>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    pub name: String,
    pub visibility: Visibility,
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
    pub is_static: bool,
    /// Parsed but not yet enforced (const-correctness lands with immutability
    /// checks in a later phase).
    pub is_const: bool,
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
    /// `new ClassName(args)`.
    New(String, Vec<Expr>),
    /// `new T[size]` — fixed-size single-dimension array creation.
    NewArray(Box<Type>, Box<Expr>),
    /// `target.field`.
    FieldAccess(Box<Expr>, String),
    /// `target.method(args)`.
    MethodCall(Box<Expr>, String, Vec<Expr>),
    /// `array[index]`.
    Index(Box<Expr>, Box<Expr>),
    /// `expr instanceof TypeName`.
    InstanceOf(Box<Expr>, String),
    PostIncr(String),
    PostDecr(String),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `match(subject) { pattern: value, ..., default: value }` — specs.md §
    /// Switch/Match. Exhaustiveness (E047) is nl-sema's job; a `None`
    /// pattern is the `default` arm and must be last.
    Match(Box<Expr>, Vec<MatchArm>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Option<Expr>,
    pub value: Expr,
}
