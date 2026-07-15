use crate::ast::*;
use crate::error::SyntaxError;
use crate::token::{Keyword, Punct, Token, TokenKind};

pub fn parse_source_file(src: &str) -> Result<SourceFile, SyntaxError> {
    let tokens = crate::lexer::Lexer::new(src).tokenize()?;
    let mut p = Parser { tokens, pos: 0 };
    p.parse_source_file()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn line(&self) -> u32 {
        self.peek().line
    }

    fn bump(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn is_punct(&self, p: Punct) -> bool {
        matches!(&self.peek().kind, TokenKind::Punct(k) if *k == p)
    }

    fn is_keyword(&self, k: Keyword) -> bool {
        matches!(&self.peek().kind, TokenKind::Keyword(kw) if *kw == k)
    }

    fn eat_punct(&mut self, p: Punct) -> Result<(), SyntaxError> {
        if self.is_punct(p) {
            self.bump();
            Ok(())
        } else {
            Err(SyntaxError::Parse(
                format!("expected {p:?}, found {:?}", self.peek().kind),
                self.line(),
            ))
        }
    }

    fn eat_keyword(&mut self, k: Keyword) -> Result<(), SyntaxError> {
        if self.is_keyword(k) {
            self.bump();
            Ok(())
        } else {
            Err(SyntaxError::Parse(
                format!("expected {k:?}, found {:?}", self.peek().kind),
                self.line(),
            ))
        }
    }

    fn eat_ident(&mut self) -> Result<String, SyntaxError> {
        match self.bump().kind {
            TokenKind::Ident(s) => Ok(s),
            other => Err(SyntaxError::Parse(
                format!("expected identifier, found {other:?}"),
                self.line(),
            )),
        }
    }

    fn parse_source_file(&mut self) -> Result<SourceFile, SyntaxError> {
        self.eat_keyword(Keyword::Namespace)?;
        let mut namespace = vec![self.eat_ident()?];
        while self.is_punct(Punct::Dot) {
            self.bump();
            namespace.push(self.eat_ident()?);
        }
        self.eat_punct(Punct::Semi)?;

        while self.is_keyword(Keyword::Use) {
            self.bump();
            loop {
                self.eat_ident()?;
                if self.is_punct(Punct::Dot) {
                    self.bump();
                    continue;
                }
                break;
            }
            self.eat_punct(Punct::Semi)?;
        }

        let class = self.parse_class_decl()?;
        Ok(SourceFile { namespace, class })
    }

    fn parse_class_decl(&mut self) -> Result<ClassDecl, SyntaxError> {
        self.eat_keyword(Keyword::Class)?;
        let name = self.eat_ident()?;
        self.eat_punct(Punct::LBrace)?;

        let mut methods = Vec::new();
        while !self.is_punct(Punct::RBrace) {
            methods.push(self.parse_method_decl()?);
        }
        self.eat_punct(Punct::RBrace)?;
        Ok(ClassDecl { name, methods })
    }

    fn parse_method_decl(&mut self) -> Result<MethodDecl, SyntaxError> {
        let mut visibility = Visibility::Public;
        let mut is_static = false;
        loop {
            if self.is_keyword(Keyword::Public) {
                self.bump();
                visibility = Visibility::Public;
            } else if self.is_keyword(Keyword::Private) {
                self.bump();
                visibility = Visibility::Private;
            } else if self.is_keyword(Keyword::Protected) {
                self.bump();
                visibility = Visibility::Protected;
            } else if self.is_keyword(Keyword::Static) {
                self.bump();
                is_static = true;
            } else {
                break;
            }
        }

        let return_type = self.parse_type()?;
        let name = self.eat_ident()?;
        self.eat_punct(Punct::LParen)?;
        let mut params = Vec::new();
        while !self.is_punct(Punct::RParen) {
            let ty = self.parse_type()?;
            let name = self.eat_ident()?;
            params.push(Param { name, ty });
            if self.is_punct(Punct::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.eat_punct(Punct::RParen)?;
        let body = self.parse_block()?;

        Ok(MethodDecl {
            name,
            visibility,
            is_static,
            return_type,
            params,
            body,
        })
    }

    fn parse_type(&mut self) -> Result<Type, SyntaxError> {
        let mut ty = match &self.peek().kind {
            TokenKind::Keyword(Keyword::Void) => {
                self.bump();
                Type::Void
            }
            TokenKind::Ident(name) => match name.as_str() {
                "int" => {
                    self.bump();
                    Type::Int
                }
                "float" => {
                    self.bump();
                    Type::Float
                }
                "bool" => {
                    self.bump();
                    Type::Bool
                }
                "byte" => {
                    self.bump();
                    Type::Byte
                }
                "string" => {
                    self.bump();
                    Type::StringT
                }
                _ => Type::Named(self.eat_ident()?),
            },
            other => {
                return Err(SyntaxError::Parse(
                    format!("expected type, found {other:?}"),
                    self.line(),
                ))
            }
        };
        while self.is_punct(Punct::LBracket) {
            self.bump();
            self.eat_punct(Punct::RBracket)?;
            ty = Type::Array(Box::new(ty));
        }
        Ok(ty)
    }

    fn parse_block(&mut self) -> Result<Block, SyntaxError> {
        self.eat_punct(Punct::LBrace)?;
        let mut stmts = Vec::new();
        while !self.is_punct(Punct::RBrace) {
            stmts.push(self.parse_stmt()?);
        }
        self.eat_punct(Punct::RBrace)?;
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        if self.is_keyword(Keyword::Return) {
            self.bump();
            if self.is_punct(Punct::Semi) {
                self.bump();
                return Ok(Stmt::Return(None));
            }
            let expr = self.parse_expr()?;
            self.eat_punct(Punct::Semi)?;
            return Ok(Stmt::Return(Some(expr)));
        }
        if self.is_keyword(Keyword::If) {
            return self.parse_if_stmt();
        }
        if self.is_keyword(Keyword::While) {
            return self.parse_while_stmt();
        }
        if self.is_keyword(Keyword::For) {
            return self.parse_for_stmt();
        }
        if self.is_keyword(Keyword::Break) {
            self.bump();
            self.eat_punct(Punct::Semi)?;
            return Ok(Stmt::Break);
        }
        if self.is_keyword(Keyword::Continue) {
            self.bump();
            self.eat_punct(Punct::Semi)?;
            return Ok(Stmt::Continue);
        }
        if self.is_punct(Punct::LBrace) {
            return Ok(Stmt::Block(self.parse_block()?));
        }
        if self.looks_like_var_decl() {
            return self.parse_var_decl();
        }
        let expr = self.parse_expr()?;
        self.eat_punct(Punct::Semi)?;
        Ok(Stmt::Expr(expr))
    }

    /// Local variable declarations start with `auto` or a primitive type
    /// keyword followed by an identifier (e.g. `int x = ...`). Anything else
    /// is parsed as an expression statement (assignment, call, ...).
    fn looks_like_var_decl(&self) -> bool {
        if self.is_keyword(Keyword::Auto) {
            return true;
        }
        if let TokenKind::Ident(name) = &self.peek().kind {
            if matches!(name.as_str(), "int" | "float" | "bool" | "byte" | "string") {
                return matches!(self.tokens.get(self.pos + 1).map(|t| &t.kind), Some(TokenKind::Ident(_)));
            }
        }
        false
    }

    fn parse_var_decl(&mut self) -> Result<Stmt, SyntaxError> {
        let ty = if self.is_keyword(Keyword::Auto) {
            self.bump();
            None
        } else {
            Some(self.parse_type()?)
        };
        let name = self.eat_ident()?;
        self.eat_punct(Punct::Assign)?;
        let init = self.parse_expr()?;
        self.eat_punct(Punct::Semi)?;
        Ok(Stmt::VarDecl { ty, name, init })
    }

    fn parse_if_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        self.eat_keyword(Keyword::If)?;
        self.eat_punct(Punct::LParen)?;
        let cond = self.parse_expr()?;
        self.eat_punct(Punct::RParen)?;
        let then_branch = self.parse_block()?;
        let else_branch = if self.is_keyword(Keyword::Else) {
            self.bump();
            if self.is_keyword(Keyword::If) {
                Some(vec![self.parse_if_stmt()?])
            } else {
                Some(self.parse_block()?)
            }
        } else {
            None
        };
        Ok(Stmt::If {
            cond,
            then_branch,
            else_branch,
        })
    }

    fn parse_while_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        self.eat_keyword(Keyword::While)?;
        self.eat_punct(Punct::LParen)?;
        let cond = self.parse_expr()?;
        self.eat_punct(Punct::RParen)?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body })
    }

    fn parse_for_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        self.eat_keyword(Keyword::For)?;
        self.eat_punct(Punct::LParen)?;

        let mut init = Vec::new();
        if !self.is_punct(Punct::Semi) {
            let ty = if self.is_keyword(Keyword::Auto) {
                self.bump();
                None
            } else {
                Some(self.parse_type()?)
            };
            loop {
                let name = self.eat_ident()?;
                self.eat_punct(Punct::Assign)?;
                let expr = self.parse_expr()?;
                init.push(Stmt::VarDecl {
                    ty: ty.clone(),
                    name,
                    init: expr,
                });
                if self.is_punct(Punct::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.eat_punct(Punct::Semi)?;

        let cond = if self.is_punct(Punct::Semi) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.eat_punct(Punct::Semi)?;

        let mut step = Vec::new();
        if !self.is_punct(Punct::RParen) {
            loop {
                step.push(self.parse_expr()?);
                if self.is_punct(Punct::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.eat_punct(Punct::RParen)?;

        let body = self.parse_block()?;
        Ok(Stmt::For {
            init,
            cond,
            step,
            body,
        })
    }

    fn parse_expr(&mut self) -> Result<Expr, SyntaxError> {
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> Result<Expr, SyntaxError> {
        let lhs = self.parse_or()?;
        let compound = match &self.peek().kind {
            TokenKind::Punct(Punct::Assign) => Some(None),
            TokenKind::Punct(Punct::PlusEq) => Some(Some(BinOp::Add)),
            TokenKind::Punct(Punct::MinusEq) => Some(Some(BinOp::Sub)),
            TokenKind::Punct(Punct::StarEq) => Some(Some(BinOp::Mul)),
            TokenKind::Punct(Punct::SlashEq) => Some(Some(BinOp::Div)),
            TokenKind::Punct(Punct::PercentEq) => Some(Some(BinOp::Mod)),
            _ => None,
        };
        let Some(op) = compound else {
            return Ok(lhs);
        };
        let Expr::Ident(name) = lhs else {
            return Err(SyntaxError::Parse(
                "invalid assignment target".to_string(),
                self.line(),
            ));
        };
        self.bump();
        let rhs = self.parse_assignment()?;
        let value = match op {
            None => rhs,
            Some(binop) => Expr::Binary(binop, Box::new(Expr::Ident(name.clone())), Box::new(rhs)),
        };
        Ok(Expr::Assign(name, Box::new(value)))
    }

    fn parse_or(&mut self) -> Result<Expr, SyntaxError> {
        let mut lhs = self.parse_and()?;
        while self.is_punct(Punct::OrOr) {
            self.bump();
            let rhs = self.parse_and()?;
            lhs = Expr::Binary(BinOp::Or, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, SyntaxError> {
        let mut lhs = self.parse_equality()?;
        while self.is_punct(Punct::AndAnd) {
            self.bump();
            let rhs = self.parse_equality()?;
            lhs = Expr::Binary(BinOp::And, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_equality(&mut self) -> Result<Expr, SyntaxError> {
        let mut lhs = self.parse_relational()?;
        loop {
            let op = if self.is_punct(Punct::EqEq) {
                BinOp::Eq
            } else if self.is_punct(Punct::NotEq) {
                BinOp::Ne
            } else {
                break;
            };
            self.bump();
            let rhs = self.parse_relational()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_relational(&mut self) -> Result<Expr, SyntaxError> {
        let mut lhs = self.parse_additive()?;
        loop {
            let op = if self.is_punct(Punct::Lt) {
                BinOp::Lt
            } else if self.is_punct(Punct::Gt) {
                BinOp::Gt
            } else if self.is_punct(Punct::Le) {
                BinOp::Le
            } else if self.is_punct(Punct::Ge) {
                BinOp::Ge
            } else {
                break;
            };
            self.bump();
            let rhs = self.parse_additive()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> Result<Expr, SyntaxError> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = if self.is_punct(Punct::Plus) {
                BinOp::Add
            } else if self.is_punct(Punct::Minus) {
                BinOp::Sub
            } else {
                break;
            };
            self.bump();
            let rhs = self.parse_multiplicative()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, SyntaxError> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = if self.is_punct(Punct::Star) {
                BinOp::Mul
            } else if self.is_punct(Punct::Slash) {
                BinOp::Div
            } else if self.is_punct(Punct::Percent) {
                BinOp::Mod
            } else {
                break;
            };
            self.bump();
            let rhs = self.parse_unary()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, SyntaxError> {
        if self.is_punct(Punct::Minus) {
            self.bump();
            let expr = self.parse_unary()?;
            return Ok(Expr::Unary(UnOp::Neg, Box::new(expr)));
        }
        if self.is_punct(Punct::Not) {
            self.bump();
            let expr = self.parse_unary()?;
            return Ok(Expr::Unary(UnOp::Not, Box::new(expr)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, SyntaxError> {
        match self.bump().kind {
            TokenKind::IntLiteral(v) => Ok(Expr::IntLit(v)),
            TokenKind::FloatLiteral(v) => Ok(Expr::FloatLit(v)),
            TokenKind::StringLiteral(s) => Ok(Expr::StringLit(s)),
            TokenKind::Keyword(Keyword::True) => Ok(Expr::BoolLit(true)),
            TokenKind::Keyword(Keyword::False) => Ok(Expr::BoolLit(false)),
            TokenKind::Keyword(Keyword::Null) => Ok(Expr::NullLit),
            TokenKind::Punct(Punct::LParen) => {
                let expr = self.parse_expr()?;
                self.eat_punct(Punct::RParen)?;
                Ok(expr)
            }
            TokenKind::Ident(name) => {
                if self.is_punct(Punct::LParen) {
                    self.bump();
                    let mut args = Vec::new();
                    while !self.is_punct(Punct::RParen) {
                        args.push(self.parse_expr()?);
                        if self.is_punct(Punct::Comma) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    self.eat_punct(Punct::RParen)?;
                    Ok(Expr::Call(name, args))
                } else if self.is_punct(Punct::PlusPlus) {
                    self.bump();
                    Ok(Expr::PostIncr(name))
                } else if self.is_punct(Punct::MinusMinus) {
                    self.bump();
                    Ok(Expr::PostDecr(name))
                } else {
                    Ok(Expr::Ident(name))
                }
            }
            other => Err(SyntaxError::Parse(
                format!("expected expression, found {other:?}"),
                self.line(),
            )),
        }
    }
}
