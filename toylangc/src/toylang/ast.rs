/// Binary operator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// A Toylang expression.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum Expr {
    IntLit(i64),
    BoolLit(bool),
    StringLit(String),
    Var(String),
    /// `Vec::new()` — IDENT "::" IDENT "(" args ")"
    StaticCall { ty: String, method: String, args: Vec<Expr> },
    /// `v.push(x)` — expr "." IDENT "(" args ")"
    MethodCall { receiver: Box<Expr>, method: String, args: Vec<Expr> },
    /// `p.x` — expr "." IDENT
    FieldAccess { receiver: Box<Expr>, field: String },
    /// `Point { x: 1, y: 2 }` — IDENT "{" field_inits "}"
    StructLit { name: String, fields: Vec<(String, Expr)> },
    /// `wrap(x)` — IDENT "(" args ")"
    FnCall { name: String, args: Vec<Expr> },
    /// `a + b`, `x * 2`
    BinaryOp { op: BinOp, left: Box<Expr>, right: Box<Expr> },
}

/// A Toylang statement.
#[derive(Clone, Debug)]
pub enum Stmt {
    Let { name: String, expr: Expr },
    ExprStmt(Expr),
}

/// A parsed Toylang function body.
#[derive(Clone, Debug)]
pub struct FnBody {
    pub stmts: Vec<Stmt>,
    pub ret: Option<Expr>, // trailing expression — becomes return value
}
