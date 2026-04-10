use super::ast::{BinOp, Expr, Block, Stmt};
use super::registry::{
    ToyField, ToyFunction, ToyParam, ToyStruct, ToylangRegistry,
};
use super::typed_ast::ResolvedType;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub enum ParseError {
    UnknownIntSuffix { suffix: String },
    UnexpectedCharacter { ch: char },
    UnexpectedToken { expected: String, got: String },
    UnexpectedTopLevelToken { got: String },
    ExpectedExpression { got: String },
    ExpectedType { got: String },
    ExpectedPointerQualifier { got: String },
    DuplicateStruct { name: String },
    DuplicateFunction { name: String },
    ReservedName { name: String },
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Clone)]
enum Token {
    Ident(String),
    LBrace,
    RBrace,
    LParen,
    RParen,
    LAngle,
    RAngle,
    Lt,        // < with spaces (comparison)
    Gt,        // > with spaces (comparison)
    LAngleEq,  // <=
    RAngleEq,  // >=
    EqEq,      // ==
    BangEq,    // !=
    Colon,
    DoubleColon, // ::
    Comma,
    Ampersand,
    Star,
    Plus,
    Minus,
    Slash,
    Arrow,     // ->
    Dot,       // .
    Semicolon, // ;
    Equals,    // =
    AmpAmp,    // &&
    PipePipe,  // ||
    IntLit(i64, ResolvedType),
    StringLit(String),
    Eof,
}

fn tokenize(src: &str) -> Result<Vec<Token>, ParseError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // Skip whitespace
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }

        // Skip line comments
        if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // Arrow -> or Minus -
        if chars[i] == '-' {
            if i + 1 < chars.len() && chars[i + 1] == '>' {
                tokens.push(Token::Arrow);
                i += 2;
            } else {
                tokens.push(Token::Minus);
                i += 1;
            }
            continue;
        }

        // DoubleColon ::
        if chars[i] == ':' && i + 1 < chars.len() && chars[i + 1] == ':' {
            tokens.push(Token::DoubleColon);
            i += 2;
            continue;
        }

        // == and !=
        if chars[i] == '=' && i + 1 < chars.len() && chars[i + 1] == '=' {
            tokens.push(Token::EqEq);
            i += 2;
            continue;
        }
        if chars[i] == '!' && i + 1 < chars.len() && chars[i + 1] == '=' {
            tokens.push(Token::BangEq);
            i += 2;
            continue;
        }

        // <= and >= (always comparison — no template meaning)
        if chars[i] == '<' && i + 1 < chars.len() && chars[i + 1] == '=' {
            tokens.push(Token::LAngleEq);
            i += 2;
            continue;
        }
        if chars[i] == '>' && i + 1 < chars.len() && chars[i + 1] == '=' {
            tokens.push(Token::RAngleEq);
            i += 2;
            continue;
        }

        // < and > — disambiguate comparison vs template syntax by whitespace
        // `a < b` (spaces) → Lt (comparison), `Vec<i32>` (no space) → LAngle (template)
        if chars[i] == '<' {
            let space_before = i > 0 && chars[i - 1].is_whitespace();
            let space_after = i + 1 < chars.len() && chars[i + 1].is_whitespace();
            tokens.push(if space_before && space_after { Token::Lt } else { Token::LAngle });
            i += 1;
            continue;
        }
        if chars[i] == '>' {
            let space_before = i > 0 && chars[i - 1].is_whitespace();
            let space_after = i + 1 >= chars.len() || chars[i + 1].is_whitespace();
            tokens.push(if space_before && space_after { Token::Gt } else { Token::RAngle });
            i += 1;
            continue;
        }

        // String literals
        if chars[i] == '"' {
            i += 1; // skip opening quote
            let start = i;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            if i < chars.len() { i += 1; } // skip closing quote
            tokens.push(Token::StringLit(s));
            continue;
        }

        // Digit sequences with optional type suffix (i32, i64, usize)
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            let value: i64 = s.parse().unwrap();
            // Check for type suffix
            if i < chars.len() && (chars[i] == 'i' || chars[i] == 'u') {
                let suf_start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let suffix: String = chars[suf_start..i].iter().collect();
                let ty = match suffix.as_str() {
                    "i32" => ResolvedType::I32,
                    "i64" => ResolvedType::I64,
                    "usize" => ResolvedType::Usize,
                    _ => return Err(ParseError::UnknownIntSuffix { suffix }),
                };
                tokens.push(Token::IntLit(value, ty));
            } else {
                // No suffix: default to i32 unless value overflows
                let ty = if value > i32::MAX as i64 || value < i32::MIN as i64 {
                    ResolvedType::I64
                } else {
                    ResolvedType::I32
                };
                tokens.push(Token::IntLit(value, ty));
            }
            continue;
        }

        // && (logical AND)
        if chars[i] == '&' && i + 1 < chars.len() && chars[i + 1] == '&' {
            tokens.push(Token::AmpAmp);
            i += 2;
            continue;
        }
        // || (logical OR)
        if chars[i] == '|' && i + 1 < chars.len() && chars[i + 1] == '|' {
            tokens.push(Token::PipePipe);
            i += 2;
            continue;
        }

        // Single-char tokens
        match chars[i] {
            '{' => { tokens.push(Token::LBrace); i += 1; }
            '}' => { tokens.push(Token::RBrace); i += 1; }
            '(' => { tokens.push(Token::LParen); i += 1; }
            ')' => { tokens.push(Token::RParen); i += 1; }
            ':' => { tokens.push(Token::Colon); i += 1; }
            ',' => { tokens.push(Token::Comma); i += 1; }
            '&' => { tokens.push(Token::Ampersand); i += 1; }
            '*' => { tokens.push(Token::Star); i += 1; }
            '+' => { tokens.push(Token::Plus); i += 1; }
            '/' => { tokens.push(Token::Slash); i += 1; }
            '.' => { tokens.push(Token::Dot); i += 1; }
            ';' => { tokens.push(Token::Semicolon); i += 1; }
            '=' => { tokens.push(Token::Equals); i += 1; }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                tokens.push(Token::Ident(chars[start..i].iter().collect()));
            }
            c => return Err(ParseError::UnexpectedCharacter { ch: c }),
        }
    }

    tokens.push(Token::Eof);
    Ok(tokens)
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    #[allow(dead_code)]
    fn peek2(&self) -> Option<&Token> {
        self.tokens.get(self.pos + 1)
    }

    fn consume(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn expect_ident(&mut self) -> Result<String, ParseError> {
        match self.consume() {
            Token::Ident(s) => Ok(s),
            t => Err(ParseError::UnexpectedToken { expected: "identifier".to_string(), got: format!("{:?}", t) }),
        }
    }

    fn expect(&mut self, expected: Token) -> Result<(), ParseError> {
        let t = self.consume();
        if t == expected {
            Ok(())
        } else {
            Err(ParseError::UnexpectedToken { expected: format!("{:?}", expected), got: format!("{:?}", t) })
        }
    }

    fn parse_program(&mut self) -> Result<ToylangRegistry, ParseError> {
        let mut structs: HashMap<String, ToyStruct> = HashMap::new();
        let mut functions: HashMap<String, ToyFunction> = HashMap::new();
        let mut imports: Vec<String> = Vec::new();
        let mut struct_names: Vec<String> = Vec::new();

        loop {
            match self.peek() {
                Token::Ident(s) if s == "use" => {
                    self.consume(); // eat "use"
                    let mut path_segments = vec![self.expect_ident()?];
                    while self.peek() == &Token::DoubleColon {
                        self.consume(); // eat "::"
                        path_segments.push(self.expect_ident()?);
                    }
                    imports.push(path_segments.join("::"));
                }
                Token::Ident(s) if s == "struct" => {
                    let (name, s) = self.parse_struct(&struct_names)?;
                    if name.starts_with("__toylang_") {
                        return Err(ParseError::ReservedName { name });
                    }
                    if structs.contains_key(&name) {
                        return Err(ParseError::DuplicateStruct { name });
                    }
                    struct_names.push(name.clone());
                    structs.insert(name, s);
                }
                Token::Ident(s) if s == "fn" => {
                    let (name, f) = self.parse_fn(&struct_names)?;
                    if name.starts_with("__toylang_") {
                        return Err(ParseError::ReservedName { name });
                    }
                    if functions.contains_key(&name) {
                        return Err(ParseError::DuplicateFunction { name });
                    }
                    functions.insert(name, f);
                }
                Token::Eof => break,
                t => return Err(ParseError::UnexpectedTopLevelToken { got: format!("{:?}", t) }),
            }
        }

        Ok(ToylangRegistry { structs, functions, imports })
    }

    fn parse_struct(&mut self, struct_names: &[String]) -> Result<(String, ToyStruct), ParseError> {
        // consume "struct"
        self.consume();
        let name = self.expect_ident()?;

        // Optional generic type params: <A, B>
        let mut type_params = Vec::new();
        if self.peek() == &Token::LAngle {
            self.consume();
            while self.peek() != &Token::RAngle && self.peek() != &Token::Eof {
                type_params.push(self.expect_ident()?);
                if self.peek() == &Token::Comma { self.consume(); }
            }
            self.expect(Token::RAngle)?;
        }

        // Include the current struct name so fields can self-reference
        let mut all_names: Vec<String> = struct_names.to_vec();
        all_names.push(name.clone());

        self.expect(Token::LBrace)?;
        let mut fields = Vec::new();
        while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
            fields.push(self.parse_field(&type_params, &all_names)?);
            // optional trailing comma
            if self.peek() == &Token::Comma {
                self.consume();
            }
        }
        self.expect(Token::RBrace)?;

        Ok((name, ToyStruct { type_params, fields }))
    }

    fn parse_field(&mut self, type_params: &[String], struct_names: &[String]) -> Result<ToyField, ParseError> {
        let name = self.expect_ident()?;
        self.expect(Token::Colon)?;
        let rust_type = self.parse_type(type_params, struct_names)?;
        Ok(ToyField { name, rust_type })
    }

    fn parse_fn(&mut self, struct_names: &[String]) -> Result<(String, ToyFunction), ParseError> {
        // consume "fn"
        self.consume();
        let name = self.expect_ident()?;

        // Optional generic type params: <T, U>
        let mut type_params = Vec::new();
        if self.peek() == &Token::LAngle {
            self.consume();
            while self.peek() != &Token::RAngle && self.peek() != &Token::Eof {
                type_params.push(self.expect_ident()?);
                if self.peek() == &Token::Comma { self.consume(); }
            }
            self.expect(Token::RAngle)?;
        }

        self.expect(Token::LParen)?;
        let params = self.parse_params(&type_params, struct_names)?;
        self.expect(Token::RParen)?;

        let return_ty = if self.peek() == &Token::Arrow {
            self.consume();
            Some(self.parse_type(&type_params, struct_names)?)
        } else {
            None
        };

        // Body-less declarations (extern functions) have no braces
        if self.peek() == &Token::LBrace {
            self.expect(Token::LBrace)?;
            let body = self.parse_fn_body(&type_params, struct_names)?;
            Ok((name, ToyFunction { type_params, params, return_ty, body: Some(body) }))
        } else {
            Ok((name, ToyFunction { type_params, params, return_ty, body: None }))
        }
    }

    fn parse_fn_body(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Block, ParseError> {
        let mut stmts = Vec::new();

        loop {
            // End of body
            if self.peek() == &Token::RBrace || self.peek() == &Token::Eof {
                self.consume(); // consume '}'
                return Ok(Block { stmts, ret: None });
            }

            // "let" statement
            if let Token::Ident(s) = self.peek() {
                if s == "let" {
                    self.consume(); // consume "let"
                    let var_name = self.expect_ident()?;
                    self.expect(Token::Equals)?;
                    let expr = self.parse_expr(type_params, struct_names)?;
                    self.expect(Token::Semicolon)?;
                    stmts.push(Stmt::Let { name: var_name, expr });
                    continue;
                }
                if s == "while" {
                    self.consume();
                    let cond = self.parse_expr(type_params, struct_names)?;
                    self.expect(Token::LBrace)?;
                    let body = self.parse_fn_body(type_params, struct_names)?;
                    stmts.push(Stmt::While { cond, body: Box::new(body) });
                    continue;
                }
            }

            // Assignment: IDENT = expr ;
            // Must check peek2 to distinguish from expressions starting with an ident
            if let Token::Ident(s) = self.peek() {
                if s != "if" && s != "true" && s != "false" && s != "else" {
                    if self.peek2() == Some(&Token::Equals) {
                        let name = self.expect_ident()?;
                        self.expect(Token::Equals)?;
                        let expr = self.parse_expr(type_params, struct_names)?;
                        self.expect(Token::Semicolon)?;
                        stmts.push(Stmt::Assign { name, expr });
                        continue;
                    }
                }
            }

            // Expression — either trailing return or stmt followed by ';'
            let expr = self.parse_expr(type_params, struct_names)?;
            // if/else expressions don't need ';' when used as statements
            let is_block_expr = matches!(expr, Expr::If { .. });
            if self.peek() == &Token::Semicolon {
                self.consume(); // consume ';'
                stmts.push(Stmt::ExprStmt(expr));
            } else if is_block_expr && self.peek() != &Token::RBrace {
                // Block expression followed by more code — treat as statement (no ';' needed)
                stmts.push(Stmt::ExprStmt(expr));
            } else {
                // trailing expression — return value
                self.expect(Token::RBrace)?;
                return Ok(Block { stmts, ret: Some(expr) });
            }
        }
    }

    fn parse_expr(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Expr, ParseError> {
        self.parse_logical_or(type_params, struct_names)
    }

    // Precedence: || < && < comparison (==, !=, <, <=, >, >=) < additive (+, -) < multiplicative (*, /)
    fn parse_logical_or(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Expr, ParseError> {
        let mut left = self.parse_logical_and(type_params, struct_names)?;
        loop {
            if self.peek() != &Token::PipePipe { break; }
            self.consume();
            let right = self.parse_logical_and(type_params, struct_names)?;
            left = Expr::BinaryOp { op: BinOp::Or, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn parse_logical_and(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Expr, ParseError> {
        let mut left = self.parse_comparison(type_params, struct_names)?;
        loop {
            if self.peek() != &Token::AmpAmp { break; }
            self.consume();
            let right = self.parse_comparison(type_params, struct_names)?;
            left = Expr::BinaryOp { op: BinOp::And, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn parse_comparison(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Expr, ParseError> {
        let mut left = self.parse_additive(type_params, struct_names)?;
        loop {
            let op = match self.peek() {
                Token::EqEq     => BinOp::Eq,
                Token::BangEq   => BinOp::Ne,
                Token::Lt       => BinOp::Lt,
                Token::LAngleEq => BinOp::Le,
                Token::Gt       => BinOp::Gt,
                Token::RAngleEq => BinOp::Ge,
                _ => break,
            };
            self.consume();
            let right = self.parse_additive(type_params, struct_names)?;
            left = Expr::BinaryOp { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    // Precedence: additive (+, -) < multiplicative (*, /)
    fn parse_additive(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Expr, ParseError> {
        let mut left = self.parse_multiplicative(type_params, struct_names)?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.consume();
            let right = self.parse_multiplicative(type_params, struct_names)?;
            left = Expr::BinaryOp { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Expr, ParseError> {
        let mut left = self.parse_postfix(type_params, struct_names)?;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                _ => break,
            };
            self.consume();
            let right = self.parse_postfix(type_params, struct_names)?;
            left = Expr::BinaryOp { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn parse_postfix(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary(type_params, struct_names)?;

        // postfix chaining: expr.method(args) or expr.field
        loop {
            if self.peek() == &Token::Dot {
                self.consume();
                let ident = self.expect_ident()?;
                if self.peek() == &Token::LParen {
                    // Method call: expr.method(args)
                    self.consume(); // consume '('
                    let args = self.parse_args(type_params, struct_names)?;
                    self.expect(Token::RParen)?;
                    expr = Expr::MethodCall {
                        receiver: Box::new(expr),
                        method: ident,
                        args,
                    };
                } else {
                    // Field access: expr.field
                    expr = Expr::FieldAccess {
                        receiver: Box::new(expr),
                        field: ident,
                    };
                }
            } else {
                break;
            }
        }

        Ok(expr)
    }

    fn parse_primary(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Expr, ParseError> {
        match self.peek().clone() {
            Token::Minus => {
                self.consume();
                let operand = self.parse_primary(type_params, struct_names)?;
                Ok(Expr::UnaryNeg(Box::new(operand)))
            }
            Token::Ampersand => {
                self.consume();
                let operand = self.parse_primary(type_params, struct_names)?;
                Ok(Expr::Ref(Box::new(operand)))
            }
            Token::StringLit(s) => {
                let s = s.clone();
                self.consume();
                Ok(Expr::StringLit(s))
            }
            Token::IntLit(n, ty) => {
                let n = n;
                let ty = ty.clone();
                self.consume();
                Ok(Expr::IntLit(n, ty))
            }
            Token::Ident(name) if name == "true" || name == "false" => {
                let val = name == "true";
                self.consume();
                Ok(Expr::BoolLit(val))
            }
            Token::Ident(ref name) if name == "if" => {
                self.consume();
                let cond = self.parse_expr(type_params, struct_names)?;
                self.expect(Token::LBrace)?;
                let then_body = self.parse_fn_body(type_params, struct_names)?;
                let else_body = if let Token::Ident(s) = self.peek() {
                    if s == "else" {
                        self.consume();
                        // Check for "else if" sugar
                        if let Token::Ident(s2) = self.peek() {
                            if s2 == "if" {
                                // Desugar: else if → else { if ... }
                                let inner_if = self.parse_expr(type_params, struct_names)?;
                                Some(Block { stmts: vec![], ret: Some(inner_if) })
                            } else {
                                self.expect(Token::LBrace)?;
                                Some(self.parse_fn_body(type_params, struct_names)?)
                            }
                        } else {
                            self.expect(Token::LBrace)?;
                            Some(self.parse_fn_body(type_params, struct_names)?)
                        }
                    } else { None }
                } else { None };
                Ok(Expr::If { cond: Box::new(cond), then_body: Box::new(then_body), else_body: else_body.map(Box::new) })
            }
            Token::Ident(name) => {
                // peek ahead to distinguish:
                //   IDENT "::" IDENT "(" -> StaticCall
                //   IDENT "{" -> StructLit (only when next non-ambiguous)
                //   IDENT otherwise -> Var
                let name = name.clone();
                self.consume(); // consume the ident

                if self.peek() == &Token::DoubleColon {
                    // StaticCall: Ty::method<type_args>(args)
                    self.consume(); // consume '::'
                    let method = self.expect_ident()?;
                    let type_args = if self.peek() == &Token::LAngle {
                        self.parse_type_arg_list(type_params, struct_names)?
                    } else {
                        vec![]
                    };
                    self.expect(Token::LParen)?;
                    let args = self.parse_args(type_params, struct_names)?;
                    self.expect(Token::RParen)?;
                    Ok(Expr::StaticCall { ty: name, method, type_args, args })
                } else if self.peek() == &Token::LBrace && struct_names.contains(&name) {
                    // StructLit: Name { field: expr, ... } — only if name is a known struct
                    let fields = self.parse_struct_lit_fields(type_params, struct_names)?;
                    Ok(Expr::StructLit { name, type_args: vec![], fields })
                } else if self.peek() == &Token::LAngle {
                    // Could be FnCall with type args or generic StructLit
                    let type_args = self.parse_type_arg_list(type_params, struct_names)?;
                    if self.peek() == &Token::LBrace && struct_names.contains(&name) {
                        // Generic StructLit: Name<T1, T2> { field: expr, ... }
                        let fields = self.parse_struct_lit_fields(type_params, struct_names)?;
                        Ok(Expr::StructLit { name, type_args, fields })
                    } else {
                        // FnCall with type args: name<T1, T2>(args)
                        self.expect(Token::LParen)?;
                        let args = self.parse_args(type_params, struct_names)?;
                        self.expect(Token::RParen)?;
                        Ok(Expr::FnCall { name, type_args, args })
                    }
                } else if self.peek() == &Token::LParen {
                    // FnCall: name(args)
                    self.consume(); // consume '('
                    let args = self.parse_args(type_params, struct_names)?;
                    self.expect(Token::RParen)?;
                    Ok(Expr::FnCall { name, type_args: vec![], args })
                } else {
                    Ok(Expr::Var(name))
                }
            }
            t => Err(ParseError::ExpectedExpression { got: format!("{:?}", t) }),
        }
    }

    /// Parse `{ field: expr, ... }` struct literal fields. Consumes the braces.
    fn parse_struct_lit_fields(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Vec<(String, Expr)>, ParseError> {
        self.expect(Token::LBrace)?;
        let mut fields = Vec::new();
        while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
            let field_name = self.expect_ident()?;
            self.expect(Token::Colon)?;
            let field_expr = self.parse_expr(type_params, struct_names)?;
            fields.push((field_name, field_expr));
            if self.peek() == &Token::Comma {
                self.consume();
            }
        }
        self.expect(Token::RBrace)?;
        Ok(fields)
    }

    /// Parse `<T1, T2>` type argument list. Consumes the `<` and `>`.
    fn parse_type_arg_list(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Vec<ResolvedType>, ParseError> {
        self.expect(Token::LAngle)?;
        let mut type_args = Vec::new();
        while self.peek() != &Token::RAngle && self.peek() != &Token::Eof {
            type_args.push(self.parse_type(type_params, struct_names)?);
            if self.peek() == &Token::Comma {
                self.consume();
            }
        }
        self.expect(Token::RAngle)?;
        Ok(type_args)
    }

    fn parse_args(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Vec<Expr>, ParseError> {
        let mut args = Vec::new();
        while self.peek() != &Token::RParen && self.peek() != &Token::Eof {
            args.push(self.parse_expr(type_params, struct_names)?);
            if self.peek() == &Token::Comma {
                self.consume();
            }
        }
        Ok(args)
    }

    fn parse_params(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Vec<ToyParam>, ParseError> {
        let mut params = Vec::new();
        while self.peek() != &Token::RParen && self.peek() != &Token::Eof {
            let name = self.expect_ident()?;
            self.expect(Token::Colon)?;
            let ty = self.parse_type(type_params, struct_names)?;
            params.push(ToyParam { name, ty });
            if self.peek() == &Token::Comma {
                self.consume();
            }
        }
        Ok(params)
    }

    /// Parse a type expression and return a ResolvedType.
    fn parse_type(&mut self, type_params: &[String], struct_names: &[String]) -> Result<ResolvedType, ParseError> {
        match self.peek().clone() {
            Token::Ampersand => {
                self.consume();
                // optional "mut" (treated same as immutable ref for now)
                if let Token::Ident(s) = self.peek() {
                    if s == "mut" {
                        self.consume();
                    }
                }
                let inner = self.parse_type(type_params, struct_names)?;
                Ok(ResolvedType::Ref { inner: Box::new(inner) })
            }
            Token::Star => {
                self.consume();
                let qualifier = self.expect_ident()?;
                if qualifier != "const" && qualifier != "mut" {
                    return Err(ParseError::ExpectedPointerQualifier { got: qualifier });
                }
                let inner = self.parse_type(type_params, struct_names)?;
                Ok(ResolvedType::Ref { inner: Box::new(inner) })
            }
            Token::Ident(s) => {
                let s = s.clone();
                self.consume();

                // Check for generic args: Vec<i32>, Pair<i32, i64>
                if self.peek() == &Token::LAngle {
                    self.consume();
                    let mut args = Vec::new();
                    while self.peek() != &Token::RAngle && self.peek() != &Token::Eof {
                        args.push(self.parse_type(type_params, struct_names)?);
                        if self.peek() == &Token::Comma {
                            self.consume();
                        }
                    }
                    self.expect(Token::RAngle)?;

                    return if struct_names.contains(&s) {
                        Ok(ResolvedType::StructRef { name: s, type_args: args })
                    } else {
                        Ok(ResolvedType::RustType { name: s, type_args: args })
                    };
                }

                // Non-generic names
                match s.as_str() {
                    "i32"   => Ok(ResolvedType::I32),
                    "i64"   => Ok(ResolvedType::I64),
                    "f64"   => Ok(ResolvedType::F64),
                    "bool"  => Ok(ResolvedType::Bool),
                    "usize" => Ok(ResolvedType::Usize),
                    _ if type_params.contains(&s) => Ok(ResolvedType::TypeParam(s)),
                    _ if struct_names.contains(&s) => Ok(ResolvedType::StructRef {
                        name: s, type_args: vec![],
                    }),
                    _ => Ok(ResolvedType::RustType { name: s, type_args: vec![] }),
                }
            }
            t => Err(ParseError::ExpectedType { got: format!("{:?}", t) }),
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn parse(src: &str) -> Result<ToylangRegistry, ParseError> {
    Parser::new(tokenize(src)?).parse_program()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_unknown_int_suffix() {
        let result = parse("fn f() -> i32 { 42i16 }");
        let Err(ParseError::UnknownIntSuffix { suffix }) = result else { panic!("expected UnknownIntSuffix") };
        assert_eq!(suffix, "i16");
    }

    #[test]
    fn test_parse_unexpected_character() {
        let result = parse("fn f() { @}");
        let Err(ParseError::UnexpectedCharacter { ch }) = result else { panic!("expected UnexpectedCharacter") };
        assert_eq!(ch, '@');
    }

    #[test]
    fn test_parse_unexpected_token() {
        // Missing colon between param name and type
        let result = parse("fn f(x i32) { 42 }");
        let Err(ParseError::UnexpectedToken { expected, .. }) = result else { panic!("expected UnexpectedToken") };
        assert_eq!(expected, "Colon");
    }

    #[test]
    fn test_parse_unexpected_top_level_token() {
        let result = parse("let x = 42;");
        assert!(matches!(result, Err(ParseError::UnexpectedTopLevelToken { .. })));
    }

    #[test]
    fn test_parse_expected_expression() {
        let result = parse("fn f() { let x = ; }");
        assert!(matches!(result, Err(ParseError::ExpectedExpression { .. })));
    }

    #[test]
    fn test_parse_expected_type() {
        let result = parse("fn f(x: ) -> i32 { 42 }");
        assert!(matches!(result, Err(ParseError::ExpectedType { .. })));
    }

    #[test]
    fn test_parse_expected_pointer_qualifier() {
        let result = parse("fn f(x: *wrong i32) { }");
        let Err(ParseError::ExpectedPointerQualifier { got }) = result else { panic!("expected ExpectedPointerQualifier") };
        assert_eq!(got, "wrong");
    }

    #[test]
    fn test_parse_duplicate_struct() {
        let result = parse("struct Foo { x: i32 } struct Foo { y: i32 }");
        let Err(ParseError::DuplicateStruct { name }) = result else { panic!("expected DuplicateStruct") };
        assert_eq!(name, "Foo");
    }

    #[test]
    fn test_parse_duplicate_function() {
        let result = parse("fn f() -> i32 { 1 } fn f() -> i32 { 2 }");
        let Err(ParseError::DuplicateFunction { name }) = result else { panic!("expected DuplicateFunction") };
        assert_eq!(name, "f");
    }

    #[test]
    fn test_parse_reserved_struct_name() {
        let result = parse("struct __toylang_foo { x: i32 }");
        let Err(ParseError::ReservedName { name }) = result else { panic!("expected ReservedName") };
        assert_eq!(name, "__toylang_foo");
    }

    #[test]
    fn test_parse_reserved_function_name() {
        let result = parse("fn __toylang_main() -> i32 { 0 }");
        let Err(ParseError::ReservedName { name }) = result else { panic!("expected ReservedName") };
        assert_eq!(name, "__toylang_main");
    }
}
