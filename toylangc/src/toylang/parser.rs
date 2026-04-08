use super::ast::{BinOp, Expr, FnBody, Stmt};
use super::registry::{
    ToyField, ToyFunction, ToyParam, ToyStruct, ToylangRegistry,
};
use super::typed_ast::ResolvedType;
use std::collections::HashMap;

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
    IntLit(i64),
    StringLit(String),
    Eof,
}

fn tokenize(src: &str) -> Vec<Token> {
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

        // Digit sequences
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            tokens.push(Token::IntLit(s.parse::<i64>().unwrap()));
            continue;
        }

        // Single-char tokens
        match chars[i] {
            '{' => { tokens.push(Token::LBrace); i += 1; }
            '}' => { tokens.push(Token::RBrace); i += 1; }
            '(' => { tokens.push(Token::LParen); i += 1; }
            ')' => { tokens.push(Token::RParen); i += 1; }
            '<' => { tokens.push(Token::LAngle); i += 1; }
            '>' => { tokens.push(Token::RAngle); i += 1; }
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
            c => panic!("toylang: unexpected character '{}' in source", c),
        }
    }

    tokens.push(Token::Eof);
    tokens
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Struct names accumulated during parsing, available to expression parsing
    /// for type argument resolution.
    struct_names: Vec<String>,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0, struct_names: Vec::new() }
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

    fn expect_ident(&mut self) -> Result<String, String> {
        match self.consume() {
            Token::Ident(s) => Ok(s),
            t => Err(format!("expected identifier, got {:?}", t)),
        }
    }

    fn expect(&mut self, expected: Token) -> Result<(), String> {
        let t = self.consume();
        if t == expected {
            Ok(())
        } else {
            Err(format!("expected {:?}, got {:?}", expected, t))
        }
    }

    fn parse_program(&mut self) -> Result<ToylangRegistry, String> {
        let mut structs: HashMap<String, ToyStruct> = HashMap::new();
        let mut functions: HashMap<String, ToyFunction> = HashMap::new();
        let mut imports: Vec<String> = Vec::new();

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
                    let (name, s) = self.parse_struct(&self.struct_names.clone())?;
                    self.struct_names.push(name.clone());
                    structs.insert(name, s);
                }
                Token::Ident(s) if s == "fn" => {
                    let sn = self.struct_names.clone();
                    let (name, f) = self.parse_fn(&sn)?;
                    functions.insert(name, f);
                }
                Token::Eof => break,
                t => return Err(format!("unexpected token {:?} at top level", t)),
            }
        }

        Ok(ToylangRegistry { structs, functions, imports })
    }

    fn parse_struct(&mut self, struct_names: &[String]) -> Result<(String, ToyStruct), String> {
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

        Ok((name.clone(), ToyStruct { name, type_params, fields }))
    }

    fn parse_field(&mut self, type_params: &[String], struct_names: &[String]) -> Result<ToyField, String> {
        let name = self.expect_ident()?;
        self.expect(Token::Colon)?;
        let rust_type = self.parse_type(type_params, struct_names)?;
        Ok(ToyField { name, rust_type })
    }

    fn parse_fn(&mut self, struct_names: &[String]) -> Result<(String, ToyFunction), String> {
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
            let body = self.parse_fn_body()?;
            Ok((name.clone(), ToyFunction { name, type_params, params, return_ty, body: Some(body) }))
        } else {
            Ok((name.clone(), ToyFunction { name, type_params, params, return_ty, body: None }))
        }
    }

    fn parse_fn_body(&mut self) -> Result<FnBody, String> {
        let mut stmts = Vec::new();

        loop {
            // End of body
            if self.peek() == &Token::RBrace || self.peek() == &Token::Eof {
                self.consume(); // consume '}'
                return Ok(FnBody { stmts, ret: None });
            }

            // "let" statement
            if let Token::Ident(s) = self.peek() {
                if s == "let" {
                    self.consume(); // consume "let"
                    let var_name = self.expect_ident()?;
                    self.expect(Token::Equals)?;
                    let expr = self.parse_expr()?;
                    self.expect(Token::Semicolon)?;
                    stmts.push(Stmt::Let { name: var_name, expr });
                    continue;
                }
            }

            // Expression — either trailing return or stmt followed by ';'
            let expr = self.parse_expr()?;
            if self.peek() == &Token::Semicolon {
                self.consume(); // consume ';'
                stmts.push(Stmt::ExprStmt(expr));
            } else {
                // trailing expression — return value
                self.expect(Token::RBrace)?;
                return Ok(FnBody { stmts, ret: Some(expr) });
            }
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_additive()
    }

    // Precedence: additive (+, -) < multiplicative (*, /)
    fn parse_additive(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.consume();
            let right = self.parse_multiplicative()?;
            left = Expr::BinaryOp { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_postfix()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                _ => break,
            };
            self.consume();
            let right = self.parse_postfix()?;
            left = Expr::BinaryOp { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_primary()?;

        // postfix chaining: expr.method(args) or expr.field
        loop {
            if self.peek() == &Token::Dot {
                self.consume();
                let ident = self.expect_ident()?;
                if self.peek() == &Token::LParen {
                    // Method call: expr.method(args)
                    self.consume(); // consume '('
                    let args = self.parse_args()?;
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

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Token::StringLit(s) => {
                let s = s.clone();
                self.consume();
                Ok(Expr::StringLit(s))
            }
            Token::IntLit(n) => {
                self.consume();
                Ok(Expr::IntLit(n))
            }
            Token::Ident(name) if name == "true" || name == "false" => {
                let val = name == "true";
                self.consume();
                Ok(Expr::BoolLit(val))
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
                    let sn = self.struct_names.clone();
                    let type_args = if self.peek() == &Token::LAngle {
                        self.parse_type_arg_list(&sn)?
                    } else {
                        vec![]
                    };
                    self.expect(Token::LParen)?;
                    let args = self.parse_args()?;
                    self.expect(Token::RParen)?;
                    Ok(Expr::StaticCall { ty: name, method, type_args, args })
                } else if self.peek() == &Token::LBrace {
                    // StructLit: Name { field: expr, ... }
                    self.consume(); // consume '{'
                    let mut fields = Vec::new();
                    while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
                        let field_name = self.expect_ident()?;
                        self.expect(Token::Colon)?;
                        let field_expr = self.parse_expr()?;
                        fields.push((field_name, field_expr));
                        if self.peek() == &Token::Comma {
                            self.consume();
                        }
                    }
                    self.expect(Token::RBrace)?;
                    Ok(Expr::StructLit { name, fields })
                } else if self.peek() == &Token::LAngle {
                    // FnCall with type args: name<T1, T2>(args)
                    let sn = self.struct_names.clone();
                    let type_args = self.parse_type_arg_list(&sn)?;
                    self.expect(Token::LParen)?;
                    let args = self.parse_args()?;
                    self.expect(Token::RParen)?;
                    Ok(Expr::FnCall { name, type_args, args })
                } else if self.peek() == &Token::LParen {
                    // FnCall: name(args)
                    self.consume(); // consume '('
                    let args = self.parse_args()?;
                    self.expect(Token::RParen)?;
                    Ok(Expr::FnCall { name, type_args: vec![], args })
                } else {
                    Ok(Expr::Var(name))
                }
            }
            t => Err(format!("expected expression, got {:?}", t)),
        }
    }

    /// Parse `<T1, T2>` type argument list. Consumes the `<` and `>`.
    fn parse_type_arg_list(&mut self, struct_names: &[String]) -> Result<Vec<ResolvedType>, String> {
        self.expect(Token::LAngle)?;
        let mut type_args = Vec::new();
        while self.peek() != &Token::RAngle && self.peek() != &Token::Eof {
            // In expression context, no type params are in scope
            type_args.push(self.parse_type(&[], struct_names)?);
            if self.peek() == &Token::Comma {
                self.consume();
            }
        }
        self.expect(Token::RAngle)?;
        Ok(type_args)
    }

    fn parse_args(&mut self) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();
        while self.peek() != &Token::RParen && self.peek() != &Token::Eof {
            args.push(self.parse_expr()?);
            if self.peek() == &Token::Comma {
                self.consume();
            }
        }
        Ok(args)
    }

    fn parse_params(&mut self, type_params: &[String], struct_names: &[String]) -> Result<Vec<ToyParam>, String> {
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
    fn parse_type(&mut self, type_params: &[String], struct_names: &[String]) -> Result<ResolvedType, String> {
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
                    return Err(format!("expected 'const' or 'mut' after '*', got '{}'", qualifier));
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
            t => Err(format!("expected type, got {:?}", t)),
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn parse(src: &str) -> Result<ToylangRegistry, String> {
    Parser::new(tokenize(src)).parse_program()
}
