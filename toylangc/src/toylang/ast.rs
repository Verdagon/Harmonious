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
    IntLit(i64, crate::toylang::typed_ast::ResolvedType),
    BoolLit(bool),
    StringLit(String),
    Var(String),
    /// `Vec::new<Point>()` — IDENT "::" IDENT "<" type_args ">" "(" args ")"
    StaticCall { ty: String, method: String, type_args: Vec<crate::toylang::typed_ast::ResolvedType>, args: Vec<Expr> },
    /// `v.push(x)` — expr "." IDENT "(" args ")"
    MethodCall { receiver: Box<Expr>, method: String, args: Vec<Expr> },
    /// `p.x` — expr "." IDENT
    FieldAccess { receiver: Box<Expr>, field: String },
    /// `Point { x: 1, y: 2 }` or `Pair<i32, i64> { first: 1, second: 2i64 }`
    StructLit { name: String, type_args: Vec<crate::toylang::typed_ast::ResolvedType>, fields: Vec<(String, Expr)> },
    /// `wrap<i32>(x)` — IDENT "<" type_args ">" "(" args ")"
    FnCall { name: String, type_args: Vec<crate::toylang::typed_ast::ResolvedType>, args: Vec<Expr> },
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
