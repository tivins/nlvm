//! Minimal AST — covers the subset of nlvm-specs/docs/specs.md needed so far
//! (namespace, single class, static methods, arithmetic/logical expressions,
//! `return`). Extended incrementally as later milestones are implemented.

#[derive(Debug, Clone, PartialEq)]
pub struct SourceFile {
    pub namespace: Vec<String>,
    pub class: ClassDecl,
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
    pub methods: Vec<MethodDecl>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodDecl {
    pub name: String,
    pub visibility: Visibility,
    pub is_static: bool,
    pub return_type: Type,
    pub params: Vec<Param>,
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
    Ident(String),
    Assign(String, Box<Expr>),
    Call(String, Vec<Expr>),
    PostIncr(String),
    PostDecr(String),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
}
