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

    fn peek_at(&self, offset: usize) -> Option<&TokenKind> {
        self.tokens.get(self.pos + offset).map(|t| &t.kind)
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

    /// Like `eat_ident`, but also accepts a reserved keyword spelled out as
    /// its source text — the only place this is needed is a namespace/`use`
    /// path segment (e.g. `test.class`, `test.instanceof`), where NL's own
    /// keywords can legally appear as plain path components.
    fn eat_namespace_segment(&mut self) -> Result<String, SyntaxError> {
        match self.bump().kind {
            TokenKind::Ident(s) => Ok(s),
            TokenKind::Keyword(kw) => Ok(kw.as_str().to_string()),
            other => Err(SyntaxError::Parse(
                format!("expected identifier, found {other:?}"),
                self.line(),
            )),
        }
    }

    fn parse_source_file(&mut self) -> Result<SourceFile, SyntaxError> {
        self.eat_keyword(Keyword::Namespace)?;
        let mut namespace = vec![self.eat_namespace_segment()?];
        while self.is_punct(Punct::Dot) {
            self.bump();
            namespace.push(self.eat_namespace_segment()?);
        }
        self.eat_punct(Punct::Semi)?;

        let mut uses = Vec::new();
        while self.is_keyword(Keyword::Use) {
            self.bump();
            let mut segments = vec![self.eat_namespace_segment()?];
            while self.is_punct(Punct::Dot) {
                self.bump();
                segments.push(self.eat_namespace_segment()?);
            }
            self.eat_punct(Punct::Semi)?;
            uses.push(segments.join("."));
        }

        let item = if self.is_keyword(Keyword::Interface) {
            SourceItem::Interface(self.parse_interface_decl()?)
        } else {
            SourceItem::Class(self.parse_class_decl()?)
        };
        Ok(SourceFile { namespace, uses, item })
    }

    fn parse_interface_decl(&mut self) -> Result<InterfaceDecl, SyntaxError> {
        self.eat_keyword(Keyword::Interface)?;
        let name = self.eat_ident()?;
        self.eat_punct(Punct::LBrace)?;
        let mut methods = Vec::new();
        while !self.is_punct(Punct::RBrace) {
            while self.is_keyword(Keyword::Public)
                || self.is_keyword(Keyword::Private)
                || self.is_keyword(Keyword::Protected)
            {
                self.bump();
            }
            let return_type = self.parse_type()?;
            let name = self.eat_ident()?;
            self.eat_punct(Punct::LParen)?;
            let params = self.parse_params()?;
            self.eat_punct(Punct::RParen)?;
            if self.is_keyword(Keyword::Const) {
                self.bump();
            }
            self.parse_throws_clause()?;
            self.eat_punct(Punct::Semi)?;
            methods.push(MethodSig { name, return_type, params });
        }
        self.eat_punct(Punct::RBrace)?;
        Ok(InterfaceDecl { name, methods })
    }

    fn parse_class_decl(&mut self) -> Result<ClassDecl, SyntaxError> {
        self.eat_keyword(Keyword::Class)?;
        let name = self.eat_ident()?;

        let extends = if self.is_keyword(Keyword::Extends) {
            self.bump();
            Some(self.eat_ident()?)
        } else {
            None
        };

        let mut implements = Vec::new();
        if self.is_keyword(Keyword::Implements) {
            self.bump();
            loop {
                implements.push(self.eat_ident()?);
                if self.is_punct(Punct::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }

        self.eat_punct(Punct::LBrace)?;
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while !self.is_punct(Punct::RBrace) {
            self.parse_member(&mut fields, &mut methods)?;
        }
        self.eat_punct(Punct::RBrace)?;
        Ok(ClassDecl { name, extends, implements, fields, methods })
    }

    /// `throws Type1, Type2, ...` — optional, after a method/constructor's
    /// parameter list and before its body (or before an interface method's
    /// trailing `;`).
    fn parse_throws_clause(&mut self) -> Result<Vec<String>, SyntaxError> {
        let mut throws = Vec::new();
        if self.is_keyword(Keyword::Throws) {
            self.bump();
            loop {
                throws.push(self.eat_ident()?);
                if self.is_punct(Punct::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        Ok(throws)
    }

    /// Parses one field, constructor, destructor, or method declaration and
    /// appends it to the relevant `Vec`.
    fn parse_member(
        &mut self,
        fields: &mut Vec<FieldDecl>,
        methods: &mut Vec<MethodDecl>,
    ) -> Result<(), SyntaxError> {
        let mut visibility = Visibility::Public;
        let mut is_static = false;
        let mut readonly = false;
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
            } else if self.is_keyword(Keyword::Readonly) {
                self.bump();
                readonly = true;
            } else {
                break;
            }
        }

        if self.is_keyword(Keyword::Construct) {
            self.bump();
            self.eat_punct(Punct::LParen)?;
            let params = self.parse_params()?;
            self.eat_punct(Punct::RParen)?;
            let throws = self.parse_throws_clause()?;
            let body = self.parse_block()?;
            methods.push(MethodDecl {
                name: "<construct>".to_string(),
                kind: MethodKind::Constructor,
                visibility,
                is_static: false,
                is_const: false,
                return_type: Type::Void,
                params,
                throws,
                body,
            });
            return Ok(());
        }
        if self.is_keyword(Keyword::Destruct) {
            self.bump();
            self.eat_punct(Punct::LParen)?;
            self.eat_punct(Punct::RParen)?;
            let body = self.parse_block()?;
            methods.push(MethodDecl {
                name: "<destruct>".to_string(),
                kind: MethodKind::Destructor,
                visibility,
                is_static: false,
                is_const: false,
                return_type: Type::Void,
                params: Vec::new(),
                throws: Vec::new(),
                body,
            });
            return Ok(());
        }

        let ty = self.parse_type()?;
        let name = self.eat_ident()?;
        if self.is_punct(Punct::LParen) {
            self.bump();
            let params = self.parse_params()?;
            self.eat_punct(Punct::RParen)?;
            let is_const = if self.is_keyword(Keyword::Const) {
                self.bump();
                true
            } else {
                false
            };
            let throws = self.parse_throws_clause()?;
            let body = self.parse_block()?;
            methods.push(MethodDecl {
                name,
                kind: MethodKind::Normal,
                visibility,
                is_static,
                is_const,
                return_type: ty,
                params,
                throws,
                body,
            });
        } else {
            let init = if self.is_punct(Punct::Assign) {
                self.bump();
                Some(self.parse_expr()?)
            } else {
                None
            };
            self.eat_punct(Punct::Semi)?;
            fields.push(FieldDecl { name, visibility, is_static, readonly, ty, init });
        }
        Ok(())
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, SyntaxError> {
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
        Ok(params)
    }

    fn parse_args(&mut self) -> Result<Vec<Expr>, SyntaxError> {
        let mut args = Vec::new();
        while !self.is_punct(Punct::RParen) {
            args.push(self.parse_expr()?);
            if self.is_punct(Punct::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(args)
    }

    /// Parses `Type1|Type2|...` — see specs.md § Union types and explicit
    /// nullable. Array suffixes bind tighter than `|`, so `string[]|null` is
    /// `(string[])|null`, not `string[](|null)`.
    fn parse_type(&mut self) -> Result<Type, SyntaxError> {
        let mut ty = self.parse_type_atom()?;
        if self.is_punct(Punct::Pipe) {
            let mut members = vec![ty];
            while self.is_punct(Punct::Pipe) {
                self.bump();
                members.push(self.parse_type_atom()?);
            }
            ty = Type::Union(members);
        }
        Ok(ty)
    }

    fn parse_type_atom(&mut self) -> Result<Type, SyntaxError> {
        let mut ty = match &self.peek().kind {
            TokenKind::Keyword(Keyword::Void) => {
                self.bump();
                Type::Void
            }
            TokenKind::Keyword(Keyword::Null) => {
                self.bump();
                Type::NullT
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

    /// Like `parse_type_atom`, but without consuming a trailing `[...]` —
    /// used after `new` where `[` introduces an array-size expression, not
    /// an empty array-type suffix.
    fn parse_new_base_type(&mut self) -> Result<Type, SyntaxError> {
        match &self.peek().kind {
            TokenKind::Ident(name) => match name.as_str() {
                "int" => {
                    self.bump();
                    Ok(Type::Int)
                }
                "float" => {
                    self.bump();
                    Ok(Type::Float)
                }
                "bool" => {
                    self.bump();
                    Ok(Type::Bool)
                }
                "byte" => {
                    self.bump();
                    Ok(Type::Byte)
                }
                "string" => {
                    self.bump();
                    Ok(Type::StringT)
                }
                _ => Ok(Type::Named(self.eat_ident()?)),
            },
            other => Err(SyntaxError::Parse(
                format!("expected type after 'new', found {other:?}"),
                self.line(),
            )),
        }
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
        if self.is_keyword(Keyword::This) && matches!(self.peek_at(1), Some(TokenKind::Punct(Punct::LParen))) {
            self.bump();
            self.eat_punct(Punct::LParen)?;
            let args = self.parse_args()?;
            self.eat_punct(Punct::RParen)?;
            self.eat_punct(Punct::Semi)?;
            return Ok(Stmt::ThisCall(args));
        }
        if self.is_keyword(Keyword::Super) && matches!(self.peek_at(1), Some(TokenKind::Punct(Punct::LParen))) {
            self.bump();
            self.eat_punct(Punct::LParen)?;
            let args = self.parse_args()?;
            self.eat_punct(Punct::RParen)?;
            self.eat_punct(Punct::Semi)?;
            return Ok(Stmt::SuperCall(args));
        }
        if self.is_keyword(Keyword::Throw) {
            self.bump();
            let expr = self.parse_expr()?;
            self.eat_punct(Punct::Semi)?;
            return Ok(Stmt::Throw(expr));
        }
        if self.is_keyword(Keyword::Try) {
            return self.parse_try_stmt();
        }
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

    /// Local variable declarations start with `auto` or a type name (a
    /// primitive keyword or a class/interface identifier) followed by an
    /// identifier, a `|` (union suffix, e.g. `string|null s = ...`), or an
    /// *empty* `[]` array-type suffix. Anything else is parsed as an
    /// expression statement (assignment, call, indexing like `a[0] = 1;`,
    /// ...) — a non-empty `[` after an identifier is indexing, not an array
    /// type, and no other statement form starts with two consecutive
    /// identifiers.
    fn looks_like_var_decl(&self) -> bool {
        if self.is_keyword(Keyword::Auto) {
            return true;
        }
        if matches!(&self.peek().kind, TokenKind::Ident(_)) {
            if matches!(
                self.peek_at(1),
                Some(TokenKind::Ident(_)) | Some(TokenKind::Punct(Punct::Pipe))
            ) {
                return true;
            }
            return matches!(
                (self.peek_at(1), self.peek_at(2)),
                (Some(TokenKind::Punct(Punct::LBracket)), Some(TokenKind::Punct(Punct::RBracket)))
            );
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
        let init = if self.is_punct(Punct::Assign) {
            self.bump();
            Some(self.parse_expr()?)
        } else {
            None
        };
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

    /// `try { ... } catch (Type name) { ... } ... finally { ... }` —
    /// specs.md § Exception handling. At least one `catch` or a `finally` is
    /// required (a bare `try {}` with neither is meaningless); the parser
    /// itself doesn't enforce that, it just produces empty vectors/`None`.
    fn parse_try_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        self.eat_keyword(Keyword::Try)?;
        let body = self.parse_block()?;
        let mut catches = Vec::new();
        while self.is_keyword(Keyword::Catch) {
            self.bump();
            self.eat_punct(Punct::LParen)?;
            let ty = self.eat_ident()?;
            let var = self.eat_ident()?;
            self.eat_punct(Punct::RParen)?;
            let catch_body = self.parse_block()?;
            catches.push(CatchClause { ty, var, body: catch_body });
        }
        let finally = if self.is_keyword(Keyword::Finally) {
            self.bump();
            Some(self.parse_block()?)
        } else {
            None
        };
        Ok(Stmt::Try { body, catches, finally })
    }

    /// `match(subject) { pattern: value, ..., default: value }` — specs.md §
    /// Switch/Match. A trailing comma after the last arm is optional.
    fn parse_match_expr(&mut self) -> Result<Expr, SyntaxError> {
        self.eat_keyword(Keyword::Match)?;
        self.eat_punct(Punct::LParen)?;
        let subject = self.parse_expr()?;
        self.eat_punct(Punct::RParen)?;
        self.eat_punct(Punct::LBrace)?;
        let mut arms = Vec::new();
        while !self.is_punct(Punct::RBrace) {
            let pattern = if self.is_keyword(Keyword::Default) {
                self.bump();
                None
            } else {
                Some(self.parse_expr()?)
            };
            self.eat_punct(Punct::Colon)?;
            let value = self.parse_expr()?;
            arms.push(MatchArm { pattern, value });
            if self.is_punct(Punct::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.eat_punct(Punct::RBrace)?;
        Ok(Expr::Match(Box::new(subject), arms))
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
                    init: Some(expr),
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
        let target = to_lvalue(lhs, self.line())?;
        self.bump();
        let rhs = self.parse_assignment()?;
        let value = match op {
            None => rhs,
            Some(binop) => {
                let LValue::Local(name) = &target else {
                    return Err(SyntaxError::Parse(
                        "compound assignment is only supported on local variables".to_string(),
                        self.line(),
                    ));
                };
                Expr::Binary(binop, Box::new(Expr::Ident(name.clone())), Box::new(rhs))
            }
        };
        Ok(Expr::Assign(target, Box::new(value)))
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
            if self.is_keyword(Keyword::Instanceof) {
                self.bump();
                let type_name = self.eat_ident()?;
                lhs = Expr::InstanceOf(Box::new(lhs), type_name);
                continue;
            }
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
        self.parse_postfix()
    }

    /// Primary / postfix precedence level: `.` member access (field or
    /// method call), `[]` indexing, chained after any primary expression.
    fn parse_postfix(&mut self) -> Result<Expr, SyntaxError> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.is_punct(Punct::Dot) {
                self.bump();
                let name = self.eat_ident()?;
                if self.is_punct(Punct::LParen) {
                    self.bump();
                    let args = self.parse_args()?;
                    self.eat_punct(Punct::RParen)?;
                    expr = Expr::MethodCall(Box::new(expr), name, args);
                } else {
                    expr = Expr::FieldAccess(Box::new(expr), name);
                }
            } else if self.is_punct(Punct::LBracket) {
                self.bump();
                let index = self.parse_expr()?;
                self.eat_punct(Punct::RBracket)?;
                expr = Expr::Index(Box::new(expr), Box::new(index));
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, SyntaxError> {
        if self.is_keyword(Keyword::New) {
            return self.parse_new_expr();
        }
        if self.is_keyword(Keyword::Match) {
            return self.parse_match_expr();
        }
        match self.bump().kind {
            TokenKind::IntLiteral(v) => Ok(Expr::IntLit(v)),
            TokenKind::FloatLiteral(v) => Ok(Expr::FloatLit(v)),
            TokenKind::StringLiteral(s) => Ok(Expr::StringLit(s)),
            TokenKind::Keyword(Keyword::True) => Ok(Expr::BoolLit(true)),
            TokenKind::Keyword(Keyword::False) => Ok(Expr::BoolLit(false)),
            TokenKind::Keyword(Keyword::Null) => Ok(Expr::NullLit),
            TokenKind::Keyword(Keyword::This) => Ok(Expr::This),
            TokenKind::Keyword(Keyword::Super) => Ok(Expr::Super),
            TokenKind::Punct(Punct::LParen) => {
                let expr = self.parse_expr()?;
                self.eat_punct(Punct::RParen)?;
                Ok(expr)
            }
            TokenKind::Ident(name) => {
                if self.is_punct(Punct::LParen) {
                    self.bump();
                    let args = self.parse_args()?;
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

    /// `new ClassName(args)` or `new T[size]` — specs.md § Arrays / § Basic
    /// class. Multi-dimensional and initializer-list forms are not yet
    /// supported.
    fn parse_new_expr(&mut self) -> Result<Expr, SyntaxError> {
        self.eat_keyword(Keyword::New)?;
        let base_ty = self.parse_new_base_type()?;
        if self.is_punct(Punct::LBracket) {
            self.bump();
            let size = self.parse_expr()?;
            self.eat_punct(Punct::RBracket)?;
            Ok(Expr::NewArray(Box::new(base_ty), Box::new(size)))
        } else if self.is_punct(Punct::LParen) {
            let Type::Named(class_name) = base_ty else {
                return Err(SyntaxError::Parse(
                    "'new' on a primitive type requires array syntax 'new T[size]'".to_string(),
                    self.line(),
                ));
            };
            self.bump();
            let args = self.parse_args()?;
            self.eat_punct(Punct::RParen)?;
            Ok(Expr::New(class_name, args))
        } else {
            Err(SyntaxError::Parse(
                "expected '(' or '[' after 'new <type>'".to_string(),
                self.line(),
            ))
        }
    }
}

fn to_lvalue(expr: Expr, line: u32) -> Result<LValue, SyntaxError> {
    match expr {
        Expr::Ident(name) => Ok(LValue::Local(name)),
        Expr::FieldAccess(target, name) => Ok(LValue::Field(target, name)),
        Expr::Index(target, index) => Ok(LValue::Index(target, index)),
        _ => Err(SyntaxError::Parse("invalid assignment target".to_string(), line)),
    }
}
