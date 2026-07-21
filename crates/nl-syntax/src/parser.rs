use crate::ast::*;
use crate::error::SyntaxError;
use crate::token::{Keyword, Punct, Token, TokenKind};

pub fn parse_source_file(src: &str, path: impl Into<String>) -> Result<SourceFile, SyntaxError> {
    let tokens = crate::lexer::Lexer::new(src).tokenize()?;
    let mut p = Parser {
        tokens,
        pos: 0,
        path: path.into(),
    };
    p.parse_source_file()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    path: String,
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

    fn col(&self) -> u32 {
        self.peek().col
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
                self.col(),
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
                self.col(),
            ))
        }
    }

    fn eat_ident(&mut self) -> Result<String, SyntaxError> {
        match self.bump().kind {
            TokenKind::Ident(s) => Ok(s),
            other => Err(SyntaxError::Parse(
                format!("expected identifier, found {other:?}"),
                self.line(),
                self.col(),
            )),
        }
    }

    /// Like `eat_ident`, but also accepts a reserved keyword spelled out as
    /// its source text. Used for namespace/`use` path segments (e.g.
    /// `test.class`, `test.instanceof`) and for a member name right after a
    /// postfix `.` (e.g. `system.text.Regex.match(...)` — `match` is
    /// otherwise a keyword, but this position can never be anything but a
    /// field/method name, so there is no ambiguity).
    fn eat_ident_or_keyword(&mut self) -> Result<String, SyntaxError> {
        match self.bump().kind {
            TokenKind::Ident(s) => Ok(s),
            TokenKind::Keyword(kw) => Ok(kw.as_str().to_string()),
            other => Err(SyntaxError::Parse(
                format!("expected identifier, found {other:?}"),
                self.line(),
                self.col(),
            )),
        }
    }

    fn parse_source_file(&mut self) -> Result<SourceFile, SyntaxError> {
        self.eat_keyword(Keyword::Namespace)?;
        let mut namespace = vec![self.eat_ident_or_keyword()?];
        while self.is_punct(Punct::Dot) {
            self.bump();
            namespace.push(self.eat_ident_or_keyword()?);
        }
        self.eat_punct(Punct::Semi)?;

        let mut uses = Vec::new();
        while self.is_keyword(Keyword::Use) {
            self.bump();
            let mut segments = vec![self.eat_ident_or_keyword()?];
            while self.is_punct(Punct::Dot) {
                self.bump();
                segments.push(self.eat_ident_or_keyword()?);
            }
            let alias = if self.is_keyword(Keyword::As) {
                self.bump();
                Some(self.eat_ident_or_keyword()?)
            } else {
                None
            };
            self.eat_punct(Punct::Semi)?;
            uses.push(UseDecl {
                path: segments.join("."),
                alias,
            });
        }

        // specs.md § Readonly, "Modifier order": `[abstract | final] class
        // [readonly] Name` — `abstract`/`final` precede `class` itself (and
        // any `template <...>` prefix). Both are accepted here (rather than
        // an `else if`) so writing both together is a semantic E049, not a
        // raw parse error.
        let mut is_abstract = false;
        let mut is_final = false;
        loop {
            if self.is_keyword(Keyword::Abstract) {
                self.bump();
                is_abstract = true;
            } else if self.is_keyword(Keyword::Final) {
                self.bump();
                is_final = true;
            } else {
                break;
            }
        }

        let item = if self.is_keyword(Keyword::Template) {
            let type_params = self.parse_template_prefix()?;
            SourceItem::Class(self.parse_class_decl(type_params, is_abstract, is_final)?)
        } else if self.is_keyword(Keyword::Interface) {
            SourceItem::Interface(self.parse_interface_decl()?)
        } else if self.is_keyword(Keyword::Enum) {
            SourceItem::Class(self.parse_enum_decl()?)
        } else {
            SourceItem::Class(self.parse_class_decl(Vec::new(), is_abstract, is_final)?)
        };

        // A .nl file holds exactly one top-level class/interface/enum. Without
        // this check, a second declaration after the first is silently
        // discarded rather than rejected (its tokens are just never parsed).
        if !matches!(self.peek().kind, TokenKind::Eof) {
            return Err(SyntaxError::Parse(
                format!(
                    "unexpected {:?} after top-level declaration: a .nl file may contain only one class, interface, or enum",
                    self.peek().kind
                ),
                self.line(),
                self.col(),
            ));
        }

        Ok(SourceFile {
            namespace,
            uses,
            item,
            path: self.path.clone(),
        })
    }

    /// `template <type T [extends Bound], ...>` before a `class` — specs.md
    /// § Template class / § Bounded type parameters. Only template *classes*
    /// are supported this phase, not template methods (a `template <...>`
    /// prefix inside a class body, before a single method, is not
    /// recognized).
    fn parse_template_prefix(&mut self) -> Result<Vec<TypeParam>, SyntaxError> {
        self.eat_keyword(Keyword::Template)?;
        self.eat_punct(Punct::Lt)?;
        let mut params = Vec::new();
        loop {
            self.eat_keyword(Keyword::TypeKw)?;
            let name = self.eat_ident()?;
            let bound = if self.is_keyword(Keyword::Extends) {
                self.bump();
                Some(self.eat_ident()?)
            } else {
                None
            };
            params.push(TypeParam { name, bound });
            if self.is_punct(Punct::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.eat_punct(Punct::Gt)?;
        Ok(params)
    }

    /// `<Type1, Type2, ...>` — concrete type arguments for a template
    /// reference (`Vector<int>`), in either a type position or after `new`.
    fn parse_generic_args(&mut self) -> Result<Vec<Type>, SyntaxError> {
        self.eat_punct(Punct::Lt)?;
        let mut args = vec![self.parse_type()?];
        while self.is_punct(Punct::Comma) {
            self.bump();
            args.push(self.parse_type()?);
        }
        self.eat_punct(Punct::Gt)?;
        Ok(args)
    }

    fn parse_interface_decl(&mut self) -> Result<InterfaceDecl, SyntaxError> {
        let decl_line = self.line();
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
            let is_const = if self.is_keyword(Keyword::Const) {
                self.bump();
                true
            } else {
                false
            };
            self.parse_throws_clause()?;
            self.eat_punct(Punct::Semi)?;
            methods.push(MethodSig {
                name,
                return_type,
                params,
                is_const,
            });
        }
        self.eat_punct(Punct::RBrace)?;
        Ok(InterfaceDecl {
            name,
            methods,
            decl_line,
        })
    }

    fn parse_class_decl(
        &mut self,
        type_params: Vec<TypeParam>,
        is_abstract: bool,
        is_final: bool,
    ) -> Result<ClassDecl, SyntaxError> {
        let decl_line = self.line();
        self.eat_keyword(Keyword::Class)?;
        // specs.md § Readonly, "Modifier order": `[abstract | final] class
        // [readonly] Name` — `readonly` follows `class`, before the name.
        let is_readonly = if self.is_keyword(Keyword::Readonly) {
            self.bump();
            true
        } else {
            false
        };
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
        Ok(ClassDecl {
            name,
            type_params,
            extends,
            implements,
            fields,
            methods,
            is_readonly,
            is_abstract,
            is_final,
            decl_line,
            is_enum: false,
            enum_cases: Vec::new(),
        })
    }

    /// `enum Name [: int|string] { case [= expr], ..., [trailing comma]
    /// [member declarations] }` — specs.md § Enums. Desugars straight into a
    /// `ClassDecl`: cases become leading `static readonly` fields of the
    /// backing type (vm.md § Enum representation), `from()`/`tryFrom()` are
    /// synthesized as ordinary static methods, and any custom
    /// static/instance methods or extra static fields are parsed exactly
    /// like a normal class body via `parse_member`.
    fn parse_enum_decl(&mut self) -> Result<ClassDecl, SyntaxError> {
        let decl_line = self.line();
        self.eat_keyword(Keyword::Enum)?;
        let name = self.eat_ident()?;

        let backing_ty = if self.is_punct(Punct::Colon) {
            self.bump();
            let ty_name = self.eat_ident()?;
            match ty_name.as_str() {
                "int" => Some(Type::Int),
                "string" => Some(Type::StringT),
                other => {
                    return Err(SyntaxError::Parse(
                        format!("enum backing type must be 'int' or 'string', found '{other}'"),
                        self.line(),
                        self.col(),
                    ))
                }
            }
        } else {
            None
        };
        let case_ty = backing_ty.clone().unwrap_or(Type::Int);
        let is_string_backed = matches!(backing_ty, Some(Type::StringT));

        self.eat_punct(Punct::LBrace)?;
        let mut raw_cases: Vec<(String, Option<Expr>)> = Vec::new();
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while !self.is_punct(Punct::RBrace) {
            // A case is a bare identifier followed by `,`, `=`, or the
            // closing `}` — anything else (a visibility/`static`/`readonly`
            // modifier, or `Type name` for an ordinary member) is parsed as
            // a regular class member instead.
            let looks_like_case = matches!(&self.peek().kind, TokenKind::Ident(_))
                && matches!(
                    self.peek_at(1),
                    Some(TokenKind::Punct(Punct::Comma))
                        | Some(TokenKind::Punct(Punct::Assign))
                        | Some(TokenKind::Punct(Punct::RBrace))
                );
            if looks_like_case {
                let case_name = self.eat_ident()?;
                let value = if self.is_punct(Punct::Assign) {
                    self.bump();
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                if value.is_none() && is_string_backed {
                    return Err(SyntaxError::Parse(
                        format!(
                            "enum case '{case_name}' of string-backed enum '{name}' requires an explicit value"
                        ),
                        self.line(),
                        self.col(),
                    ));
                }
                raw_cases.push((case_name, value));
                if self.is_punct(Punct::Comma) {
                    self.bump();
                }
            } else {
                self.parse_member(&mut fields, &mut methods)?;
            }
        }
        self.eat_punct(Punct::RBrace)?;

        let mut next_ordinal: i64 = 0;
        let mut case_fields = Vec::with_capacity(raw_cases.len());
        let mut case_names = Vec::with_capacity(raw_cases.len());
        for (case_name, value) in raw_cases {
            let init_expr = value.unwrap_or(Expr::IntLit(next_ordinal));
            if let Expr::IntLit(n) = &init_expr {
                next_ordinal = n + 1;
            }
            case_names.push(case_name.clone());
            case_fields.push(FieldDecl {
                name: case_name,
                visibility: Visibility::Public,
                visibility_explicit: true,
                is_static: true,
                readonly: true,
                ty: case_ty.clone(),
                init: Some(init_expr),
            });
        }
        case_fields.extend(fields);

        if !methods.iter().any(|m: &MethodDecl| m.name == "from") {
            methods.push(enum_from_method(&name, &case_names, false, decl_line));
        }
        if !methods.iter().any(|m: &MethodDecl| m.name == "tryFrom") {
            methods.push(enum_from_method(&name, &case_names, true, decl_line));
        }

        Ok(ClassDecl {
            name,
            type_params: Vec::new(),
            extends: None,
            implements: Vec::new(),
            fields: case_fields,
            methods,
            is_readonly: false,
            is_abstract: false,
            is_final: false,
            decl_line,
            is_enum: true,
            enum_cases: case_names,
        })
    }

    /// `throws Type1, Type2, ...` — optional, after a method/constructor's
    /// parameter list and before its body (or before an interface method's
    /// trailing `;`).
    /// `ident(.ident)*` — a possibly namespace-qualified class name. Only
    /// used in positions where `.` cannot mean anything else (types, `new`,
    /// `catch`, `throws`), never in expression position where `.` starts a
    /// field/method access.
    fn parse_dotted_name(&mut self) -> Result<String, SyntaxError> {
        let mut name = self.eat_ident()?;
        while self.is_punct(Punct::Dot) {
            self.bump();
            let seg = self.eat_ident()?;
            name.push('.');
            name.push_str(&seg);
        }
        Ok(name)
    }

    fn parse_throws_clause(&mut self) -> Result<Vec<String>, SyntaxError> {
        let mut throws = Vec::new();
        if self.is_keyword(Keyword::Throws) {
            self.bump();
            loop {
                throws.push(self.parse_dotted_name()?);
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
        let decl_line = self.line();
        let mut visibility = Visibility::Public;
        let mut visibility_explicit = false;
        let mut is_static = false;
        let mut readonly = false;
        let mut is_abstract = false;
        let mut is_final = false;
        let mut is_nodiscard = false;
        loop {
            if self.is_keyword(Keyword::Public) {
                self.bump();
                visibility = Visibility::Public;
                visibility_explicit = true;
            } else if self.is_keyword(Keyword::Private) {
                self.bump();
                visibility = Visibility::Private;
                visibility_explicit = true;
            } else if self.is_keyword(Keyword::Protected) {
                self.bump();
                visibility = Visibility::Protected;
                visibility_explicit = true;
            } else if self.is_keyword(Keyword::Static) {
                self.bump();
                is_static = true;
            } else if self.is_keyword(Keyword::Readonly) {
                self.bump();
                readonly = true;
            } else if self.is_keyword(Keyword::Abstract) {
                self.bump();
                is_abstract = true;
            } else if self.is_keyword(Keyword::Final) {
                self.bump();
                is_final = true;
            } else if self.is_keyword(Keyword::Nodiscard) {
                self.bump();
                is_nodiscard = true;
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
                visibility_explicit,
                is_static: false,
                is_const: false,
                is_abstract: false,
                is_final: false,
                is_nodiscard: false,
                return_type: Type::Void,
                params,
                throws,
                body,
                decl_line,
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
                visibility_explicit,
                is_static: false,
                is_const: false,
                is_abstract: false,
                is_final: false,
                is_nodiscard: false,
                return_type: Type::Void,
                params: Vec::new(),
                throws: Vec::new(),
                body,
                decl_line,
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
            // specs.md § Abstract classes and methods — an abstract method
            // has no body, just `;`. Providing a body anyway is accepted
            // here (rather than a raw parse error) so nl-sema can reject it
            // as a proper compile error (compiler.md, E034).
            let body = if is_abstract && self.is_punct(Punct::Semi) {
                self.bump();
                Vec::new()
            } else {
                self.parse_block()?
            };
            methods.push(MethodDecl {
                name,
                kind: MethodKind::Normal,
                visibility,
                visibility_explicit,
                is_static,
                is_const,
                is_abstract,
                is_final,
                is_nodiscard,
                return_type: ty,
                params,
                throws,
                body,
                decl_line,
            });
        } else {
            let init = if self.is_punct(Punct::Assign) {
                self.bump();
                Some(self.parse_expr()?)
            } else {
                None
            };
            self.eat_punct(Punct::Semi)?;
            fields.push(FieldDecl {
                name,
                visibility,
                visibility_explicit,
                is_static,
                readonly,
                ty,
                init,
            });
        }
        Ok(())
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, SyntaxError> {
        let mut params = Vec::new();
        while !self.is_punct(Punct::RParen) {
            let is_const = if self.is_keyword(Keyword::Const) {
                self.bump();
                true
            } else {
                false
            };
            // `const ref T param` — specs.md § Ref parameters, always
            // `const` before `ref` when both are present.
            let is_ref = if self.is_keyword(Keyword::Ref) {
                self.bump();
                true
            } else {
                false
            };
            let ty = self.parse_type()?;
            let name = self.eat_ident()?;
            let default = self.parse_param_default()?;
            params.push(Param {
                name,
                ty,
                is_const,
                default,
                is_ref,
            });
            if self.is_punct(Punct::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(params)
    }

    /// `= expr` — specs.md § Optional parameters. Shared by `parse_params`
    /// and `parse_closure`'s inline param loop.
    fn parse_param_default(&mut self) -> Result<Option<Expr>, SyntaxError> {
        if self.is_punct(Punct::Assign) {
            self.bump();
            Ok(Some(self.parse_expr()?))
        } else {
            Ok(None)
        }
    }

    fn parse_args(&mut self) -> Result<Vec<Arg>, SyntaxError> {
        let mut args = Vec::new();
        while !self.is_punct(Punct::RParen) {
            args.push(self.parse_arg()?);
            if self.is_punct(Punct::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(args)
    }

    /// One call-site argument — `expr`, `name: expr` (specs.md § Named
    /// parameters), or either prefixed with `ref` (specs.md § Ref
    /// parameters, e.g. `Utils.swap(ref x, ref y)`, or `foo(a: ref x)`
    /// combining both). The named form is recognized by a bare identifier
    /// immediately followed by `:` — distinct from the ternary
    /// `cond ? a : b`'s `:` (always preceded by a `?`, never adjacent to
    /// the identifier) and from `foreach`/`match`'s `:` (not reachable
    /// from `parse_expr`, which is the only thing called here). `nl-sema`
    /// validates ordering/binding/ref-correctness (E020-E026); the parser
    /// accepts any mix.
    fn parse_arg(&mut self) -> Result<Arg, SyntaxError> {
        let name = if let TokenKind::Ident(name) = &self.peek().kind {
            if matches!(self.peek_at(1), Some(TokenKind::Punct(Punct::Colon))) {
                let name = name.clone();
                self.bump();
                self.bump();
                Some(name)
            } else {
                None
            }
        } else {
            None
        };
        let is_ref = if self.is_keyword(Keyword::Ref) {
            self.bump();
            true
        } else {
            false
        };
        Ok(Arg {
            name,
            is_ref,
            value: self.parse_expr()?,
        })
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
            // `(Type, Type, ...) => ReturnType [throws Name, ...]` —
            // specs.md § Function type assignment. Unambiguous here: `(`
            // never starts any other type-atom form, so no lookahead is
            // needed inside a type position (contrast with statement
            // position, where `looks_like_function_type_start` disambiguates
            // this from a parenthesized/closure expression).
            TokenKind::Punct(Punct::LParen) => {
                self.bump();
                let mut params = Vec::new();
                while !self.is_punct(Punct::RParen) {
                    params.push(self.parse_type()?);
                    if self.is_punct(Punct::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
                self.eat_punct(Punct::RParen)?;
                self.eat_punct(Punct::FatArrow)?;
                let return_type = Box::new(self.parse_type()?);
                let throws = self.parse_throws_clause()?;
                Type::Function {
                    params,
                    return_type,
                    throws,
                }
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
                _ => {
                    // Namespace-qualified class name (`system.io.IOException`
                    // in a catch clause, `system.io.FileHandle` as a local's
                    // type, ...). Unambiguous in type position: nothing else
                    // can follow an identifier with `.` here.
                    let name = self.parse_dotted_name()?;
                    if self.is_punct(Punct::Lt) {
                        Type::Generic(name, self.parse_generic_args()?)
                    } else {
                        Type::Named(name)
                    }
                }
            },
            other => {
                return Err(SyntaxError::Parse(
                    format!("expected type, found {other:?}"),
                    self.line(),
                self.col(),
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
                _ => {
                    // Dotted namespace-qualified class name — needed for
                    // `new system.List<int>(...)`/`new system.Map<K,V>(...)`
                    // (stdlib.md § system.List/system.Map). Only `new`
                    // itself follows this position (`<`, `[`, or `(`), so
                    // greedily consuming `.`-separated segments is
                    // unambiguous here, unlike in expression position where
                    // `.` starts a field/method access.
                    let name = self.parse_dotted_name()?;
                    if self.is_punct(Punct::Lt) {
                        Ok(Type::Generic(name, self.parse_generic_args()?))
                    } else {
                        Ok(Type::Named(name))
                    }
                }
            },
            other => Err(SyntaxError::Parse(
                format!("expected type after 'new', found {other:?}"),
                self.line(),
                self.col(),
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
        let line = self.line();
        if self.is_keyword(Keyword::This)
            && matches!(self.peek_at(1), Some(TokenKind::Punct(Punct::LParen)))
        {
            self.bump();
            self.eat_punct(Punct::LParen)?;
            let args = self.parse_args()?;
            self.eat_punct(Punct::RParen)?;
            self.eat_punct(Punct::Semi)?;
            return Ok(Stmt {
                kind: StmtKind::ThisCall(args),
                line,
            });
        }
        if self.is_keyword(Keyword::Super)
            && matches!(self.peek_at(1), Some(TokenKind::Punct(Punct::LParen)))
        {
            self.bump();
            self.eat_punct(Punct::LParen)?;
            let args = self.parse_args()?;
            self.eat_punct(Punct::RParen)?;
            self.eat_punct(Punct::Semi)?;
            return Ok(Stmt {
                kind: StmtKind::SuperCall(args),
                line,
            });
        }
        if self.is_keyword(Keyword::Throw) {
            self.bump();
            let expr = self.parse_expr()?;
            self.eat_punct(Punct::Semi)?;
            return Ok(Stmt {
                kind: StmtKind::Throw(expr),
                line,
            });
        }
        if self.is_keyword(Keyword::Try) {
            return self.parse_try_stmt();
        }
        if self.is_keyword(Keyword::Return) {
            self.bump();
            if self.is_punct(Punct::Semi) {
                self.bump();
                return Ok(Stmt {
                    kind: StmtKind::Return(None),
                    line,
                });
            }
            let expr = self.parse_expr()?;
            self.eat_punct(Punct::Semi)?;
            return Ok(Stmt {
                kind: StmtKind::Return(Some(expr)),
                line,
            });
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
            return Ok(Stmt {
                kind: StmtKind::Break,
                line,
            });
        }
        if self.is_keyword(Keyword::Continue) {
            self.bump();
            self.eat_punct(Punct::Semi)?;
            return Ok(Stmt {
                kind: StmtKind::Continue,
                line,
            });
        }
        if self.is_punct(Punct::LBrace) {
            return Ok(Stmt {
                kind: StmtKind::Block(self.parse_block()?),
                line,
            });
        }
        // `const T name = expr;` — compiler.md § Const local variables
        // (E012). A leading `const` in statement position is unambiguous:
        // it never starts any other statement form.
        if self.is_keyword(Keyword::Const) {
            self.bump();
            return self.parse_var_decl(true, line);
        }
        if self.looks_like_var_decl() {
            return self.parse_var_decl(false, line);
        }
        let expr = self.parse_expr()?;
        self.eat_punct(Punct::Semi)?;
        Ok(Stmt {
            kind: StmtKind::Expr(expr),
            line,
        })
    }

    /// Local variable declarations start with `auto` or a type name (a
    /// primitive keyword or a class/interface identifier) followed by an
    /// identifier, a `|` (union suffix, e.g. `string|null s = ...`), or an
    /// *empty* `[]` array-type suffix. Anything else is parsed as an
    /// expression statement (assignment, call, indexing like `a[0] = 1;`,
    /// ...) — a non-empty `[` after an identifier is indexing, not an array
    /// type, and no other statement form starts with two consecutive
    /// identifiers.
    ///
    /// A leading `(` is the one additional case that needs real lookahead
    /// (`looks_like_function_type_start`) rather than a token peek: it's
    /// ambiguous with a parenthesized expression statement or a niladic
    /// closure expression statement — see that function's doc comment.
    fn looks_like_var_decl(&mut self) -> bool {
        if self.is_keyword(Keyword::Auto) {
            return true;
        }
        if self.is_punct(Punct::LParen) {
            return self.looks_like_function_type_start();
        }
        if matches!(&self.peek().kind, TokenKind::Ident(_)) {
            // Skip a dotted qualified-name prefix (`system.io.FileHandle h`)
            // — a `.` immediately followed by an identifier can only
            // continue either a type name or a field access, and the checks
            // below (next token is an identifier, `|`, `<...>`, or `[]`)
            // only ever match the type-name reading: an expression statement
            // after `a.b` continues with `(`, `=`, `;`, an operator, ...
            let mut offset = 1usize;
            while matches!(self.peek_at(offset), Some(TokenKind::Punct(Punct::Dot)))
                && matches!(self.peek_at(offset + 1), Some(TokenKind::Ident(_)))
            {
                offset += 2;
            }
            if matches!(
                self.peek_at(offset),
                Some(TokenKind::Ident(_)) | Some(TokenKind::Punct(Punct::Pipe))
            ) {
                return true;
            }
            if matches!(self.peek_at(offset), Some(TokenKind::Punct(Punct::Lt))) {
                return self.looks_like_generic_type_decl(offset);
            }
            return matches!(
                (self.peek_at(offset), self.peek_at(offset + 1)),
                (
                    Some(TokenKind::Punct(Punct::LBracket)),
                    Some(TokenKind::Punct(Punct::RBracket))
                )
            );
        }
        false
    }

    /// Tentatively parses a function type (`(Type, ...) => ReturnType
    /// [throws ...]`) at the current position and checks it's immediately
    /// followed by an identifier — the variable name of `(int) => int f =
    /// ...;`. Always restores `self.pos`, so this is pure lookahead despite
    /// reusing the real recursive-descent `parse_type_atom`.
    ///
    /// This is the only shape that distinguishes a function-type var decl
    /// from the two other things a statement-position `(` can start:
    /// a parenthesized expression statement (`(a + b).toString();`) and a
    /// niladic closure expression statement (`() => { ... };`). Both of
    /// those fail this tentative parse before ever reaching an identifier —
    /// a closure's parameter list pairs each type with a *name*
    /// (`(int a) => ...`), which a bare type list can never parse past the
    /// first parameter, and a closure's body starts with `{` or an
    /// expression, never an identifier standing alone.
    fn looks_like_function_type_start(&mut self) -> bool {
        let save = self.pos;
        let result = self.parse_type_atom().is_ok() && matches!(self.peek().kind, TokenKind::Ident(_));
        self.pos = save;
        result
    }

    /// `Name<...> ident` — e.g. `Box<int> a`. Without this lookahead,
    /// `Box<int>` is indistinguishable from the chained relational
    /// expression `Box < int > a`; scans forward from the `<` at
    /// `lt_offset`, tracking nesting depth (for `Box<Box<int>>`), bailing
    /// out (not a generic type) on any token that couldn't plausibly appear
    /// inside a type-argument list.
    fn looks_like_generic_type_decl(&self, lt_offset: usize) -> bool {
        let mut depth = 0i32;
        let mut offset = lt_offset;
        loop {
            match self.peek_at(offset) {
                Some(TokenKind::Punct(Punct::Lt)) => depth += 1,
                Some(TokenKind::Punct(Punct::Gt)) => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.peek_at(offset + 1), Some(TokenKind::Ident(_)));
                    }
                }
                Some(TokenKind::Ident(_))
                | Some(TokenKind::Punct(Punct::Comma))
                | Some(TokenKind::Punct(Punct::Dot))
                | Some(TokenKind::Punct(Punct::LBracket))
                | Some(TokenKind::Punct(Punct::RBracket))
                | Some(TokenKind::Punct(Punct::Pipe)) => {}
                _ => return false,
            }
            offset += 1;
            if offset > 64 {
                return false;
            }
        }
    }

    fn parse_var_decl(&mut self, is_const: bool, line: u32) -> Result<Stmt, SyntaxError> {
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
        Ok(Stmt {
            kind: StmtKind::VarDecl {
                ty,
                name,
                init,
                is_const,
            },
            line,
        })
    }

    fn parse_if_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        let line = self.line();
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
        Ok(Stmt {
            kind: StmtKind::If {
                cond,
                then_branch,
                else_branch,
            },
            line,
        })
    }

    /// Speculative for-each header parse — `Ok(None)` means "not a
    /// for-each, rewind and parse a C-style header" (the caller restores
    /// `self.pos`); anything after the committing `:` reports real errors.
    fn try_parse_foreach_header(&mut self) -> Result<Option<Stmt>, SyntaxError> {
        let line = self.line();
        if self.is_keyword(Keyword::Const) {
            self.bump();
        }
        let ty = if self.is_keyword(Keyword::Auto) {
            self.bump();
            None
        } else {
            match self.parse_type() {
                Ok(t) => Some(t),
                Err(_) => return Ok(None),
            }
        };
        let Ok(var) = self.eat_ident() else {
            return Ok(None);
        };
        if !self.is_punct(Punct::Colon) {
            return Ok(None);
        }
        self.bump();
        let iterable = self.parse_expr()?;
        self.eat_punct(Punct::RParen)?;
        let body = self.parse_block()?;
        Ok(Some(Stmt {
            kind: StmtKind::ForEach {
                ty,
                var,
                iterable,
                body,
            },
            line,
        }))
    }

    /// `try { ... } catch (Type name) { ... } ... finally { ... }` —
    /// specs.md § Exception handling. At least one `catch` or a `finally` is
    /// required (a bare `try {}` with neither is meaningless); the parser
    /// itself doesn't enforce that, it just produces empty vectors/`None`.
    fn parse_try_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        let line = self.line();
        self.eat_keyword(Keyword::Try)?;
        let body = self.parse_block()?;
        let mut catches = Vec::new();
        while self.is_keyword(Keyword::Catch) {
            self.bump();
            self.eat_punct(Punct::LParen)?;
            let ty = self.parse_dotted_name()?;
            let var = self.eat_ident()?;
            self.eat_punct(Punct::RParen)?;
            let catch_body = self.parse_block()?;
            catches.push(CatchClause {
                ty,
                var,
                body: catch_body,
            });
        }
        let finally = if self.is_keyword(Keyword::Finally) {
            self.bump();
            Some(self.parse_block()?)
        } else {
            None
        };
        Ok(Stmt {
            kind: StmtKind::Try {
                body,
                catches,
                finally,
            },
            line,
        })
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
        let line = self.line();
        self.eat_keyword(Keyword::While)?;
        self.eat_punct(Punct::LParen)?;
        let cond = self.parse_expr()?;
        self.eat_punct(Punct::RParen)?;
        let body = self.parse_block()?;
        Ok(Stmt {
            kind: StmtKind::While { cond, body },
            line,
        })
    }

    fn parse_for_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        let line = self.line();
        self.eat_keyword(Keyword::For)?;
        self.eat_punct(Punct::LParen)?;

        // For-each form: `for ([const] (auto|Type) ident : expr)` — tried
        // speculatively with rollback (same pattern as `parse_closure`),
        // since only the `:` after the loop variable distinguishes it from
        // a C-style header like `for (int i = 0; ...)`.
        let save = self.pos;
        if let Some(stmt) = self.try_parse_foreach_header()? {
            return Ok(stmt);
        }
        self.pos = save;

        let mut init = Vec::new();
        if !self.is_punct(Punct::Semi) {
            let ty = if self.is_keyword(Keyword::Auto) {
                self.bump();
                None
            } else {
                Some(self.parse_type()?)
            };
            loop {
                let init_line = self.line();
                let name = self.eat_ident()?;
                self.eat_punct(Punct::Assign)?;
                let expr = self.parse_expr()?;
                init.push(Stmt {
                    kind: StmtKind::VarDecl {
                        ty: ty.clone(),
                        name,
                        init: Some(expr),
                        is_const: false,
                    },
                    line: init_line,
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
        Ok(Stmt {
            kind: StmtKind::For {
                init,
                cond,
                step,
                body,
            },
            line,
        })
    }

    fn parse_expr(&mut self) -> Result<Expr, SyntaxError> {
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> Result<Expr, SyntaxError> {
        let lhs = self.parse_coalesce()?;
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
        let target = to_lvalue(lhs, self.line(), self.col())?;
        self.bump();
        let rhs = self.parse_assignment()?;
        let value = match op {
            None => rhs,
            Some(binop) => {
                let LValue::Local(name) = &target else {
                    return Err(SyntaxError::Parse(
                        "compound assignment is only supported on local variables".to_string(),
                        self.line(),
                self.col(),
                    ));
                };
                Expr::Binary(binop, Box::new(Expr::Ident(name.clone())), Box::new(rhs))
            }
        };
        Ok(Expr::Assign(target, Box::new(value)))
    }

    /// specs.md § Operator precedence, level 11 — `??` and `?:` (elvis),
    /// left-associative, looser than ternary (level 10) and tighter than
    /// assignment. Both share one precedence level and are parsed in the
    /// same left-to-right loop (e.g. `a ?? b ?: c` parses as
    /// `(a ?? b) ?: c`). Each operand is itself a full ternary, so
    /// `a ?? b ? c : d` parses as `a ?? (b ? c : d)` per specs.md's worked
    /// example (the `b ? c : d` ternary is parsed whole as the `??`
    /// right-hand operand).
    fn parse_coalesce(&mut self) -> Result<Expr, SyntaxError> {
        let mut lhs = self.parse_ternary()?;
        loop {
            if self.is_punct(Punct::QuestionQuestion) {
                self.bump();
                let rhs = self.parse_ternary()?;
                lhs = Expr::Coalesce(Box::new(lhs), Box::new(rhs));
            } else if self.is_punct(Punct::QuestionColon) {
                self.bump();
                let rhs = self.parse_ternary()?;
                lhs = Expr::Elvis(Box::new(lhs), Box::new(rhs));
            } else {
                break;
            }
        }
        Ok(lhs)
    }

    /// specs.md § Operator precedence, level 10 — `cond ? then : else`,
    /// right-associative, binds tighter than `??`/`?:` and looser than
    /// `||`. The `then` branch accepts a full expression (as in C/Java);
    /// the `else` branch recurses here so `a ? b : c ? d : e` nests to the
    /// right.
    fn parse_ternary(&mut self) -> Result<Expr, SyntaxError> {
        let cond = self.parse_or()?;
        if !self.is_punct(Punct::Question) {
            return Ok(cond);
        }
        self.bump();
        let then_branch = self.parse_expr()?;
        self.eat_punct(Punct::Colon)?;
        let else_branch = self.parse_ternary()?;
        Ok(Expr::Ternary(
            Box::new(cond),
            Box::new(then_branch),
            Box::new(else_branch),
        ))
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
        let mut lhs = self.parse_spaceship()?;
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
            let rhs = self.parse_spaceship()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    /// specs.md § Operator precedence, level 5 — `<=>` (three-way
    /// comparison), between additive (level 4, binds tighter) and
    /// relational/`instanceof` (level 6, binds looser).
    fn parse_spaceship(&mut self) -> Result<Expr, SyntaxError> {
        let mut lhs = self.parse_additive()?;
        while self.is_punct(Punct::Spaceship) {
            self.bump();
            let rhs = self.parse_additive()?;
            lhs = Expr::Binary(BinOp::Cmp3, Box::new(lhs), Box::new(rhs));
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
        // `(T) expr` — specs.md § Operator precedence puts cast at the same
        // level as the unary operators above. Tried before falling through
        // to `parse_postfix` (which itself tries a closure before plain
        // `(expr)` grouping) since `(` starts all three forms.
        if self.is_punct(Punct::LParen) {
            if let Some(cast) = self.try_parse_cast()? {
                return Ok(cast);
            }
        }
        self.parse_postfix()
    }

    /// Attempts `(T) expr` at the current position, restoring `self.pos`
    /// and returning `None` on any failure — same backtracking strategy as
    /// `try_parse_closure`, and for the same reason: there is no bounded
    /// lookahead that distinguishes `(T) expr` from a parenthesized
    /// expression that merely happens to start with a type-shaped
    /// identifier (`(a) - b`, where `a` could be a variable), or from a
    /// closure (`(int a) => ...`, which `parse_postfix`/`parse_primary`
    /// handle if this returns `None`).
    fn try_parse_cast(&mut self) -> Result<Option<Expr>, SyntaxError> {
        let save = self.pos;
        match self.parse_cast() {
            Ok(Some(cast)) => Ok(Some(cast)),
            Ok(None) | Err(_) => {
                self.pos = save;
                Ok(None)
            }
        }
    }

    fn parse_cast(&mut self) -> Result<Option<Expr>, SyntaxError> {
        self.eat_punct(Punct::LParen)?;
        let ty = self.parse_type()?;
        self.eat_punct(Punct::RParen)?;
        if !self.can_start_cast_operand(&ty) {
            return Ok(None);
        }
        let operand = self.parse_unary()?;
        Ok(Some(Expr::Cast(Box::new(ty), Box::new(operand))))
    }

    /// Whether the token right after a tentative `(T)` can only start a new
    /// unary/primary expression — ruling out treating `(name)` as a cast
    /// when what actually follows is a binary operator continuing `name` as
    /// a parenthesized *value* (`(a) - b` must parse as subtraction, not as
    /// `(a)` cast onto unary `-b`). This is the same disambiguation C
    /// resolves with a typedef table this parser doesn't have; primitive
    /// numeric/`bool`/`string` target types are exempt from excluding a
    /// leading `-` because nothing in this grammar produces a bare
    /// parenthesized value of exactly that shape, so e.g. `(byte) -1` is
    /// unambiguous.
    fn can_start_cast_operand(&self, ty: &Type) -> bool {
        let allow_leading_minus = matches!(
            ty,
            Type::Int | Type::Float | Type::Byte | Type::Bool | Type::StringT
        );
        match &self.peek().kind {
            TokenKind::Ident(_)
            | TokenKind::IntLiteral(_)
            | TokenKind::FloatLiteral(_)
            | TokenKind::StringLiteral(_) => true,
            TokenKind::Keyword(
                Keyword::True
                | Keyword::False
                | Keyword::Null
                | Keyword::This
                | Keyword::Super
                | Keyword::New
                | Keyword::Match,
            ) => true,
            TokenKind::Punct(Punct::LParen | Punct::Not) => true,
            TokenKind::Punct(Punct::Minus) => allow_leading_minus,
            _ => false,
        }
    }

    /// Primary / postfix precedence level: `.` member access (field or
    /// method call), `[]` indexing, chained after any primary expression.
    fn parse_postfix(&mut self) -> Result<Expr, SyntaxError> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.is_punct(Punct::Dot) {
                self.bump();
                let name = self.eat_ident_or_keyword()?;
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
        // `(` is otherwise only a parenthesized-expression grouping (below)
        // — a closure is the one other thing it can start, disambiguated by
        // tentatively parsing one and backtracking on failure.
        if self.is_punct(Punct::LParen) {
            if let Some(closure) = self.try_parse_closure()? {
                return Ok(closure);
            }
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
                self.col(),
            )),
        }
    }

    /// `new ClassName(args)`, `new T[n1][n2]...` or `new T[]{ e0, e1, ... }`
    /// — specs.md § Arrays / § Basic class. The initializer-list form is
    /// only recognized as a single dimension (immediately `[]{`); any other
    /// shape falls into the general multi-dimensional case, where each
    /// `[]` records an omitted size (`None`) — compiler.md §
    /// Multidimensional array creation.
    fn parse_new_expr(&mut self) -> Result<Expr, SyntaxError> {
        self.eat_keyword(Keyword::New)?;
        let base_ty = self.parse_new_base_type()?;
        if self.is_punct(Punct::LBracket) {
            self.bump();
            if self.is_punct(Punct::RBracket) {
                self.bump();
                if self.is_punct(Punct::LBrace) {
                    self.bump();
                    let mut elements = Vec::new();
                    while !self.is_punct(Punct::RBrace) {
                        elements.push(self.parse_expr()?);
                        if self.is_punct(Punct::Comma) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    self.eat_punct(Punct::RBrace)?;
                    return Ok(Expr::NewArrayInit(Box::new(base_ty), elements));
                }
                let mut dims = vec![None];
                self.parse_extra_array_dims(&mut dims)?;
                return Ok(Expr::NewArray(Box::new(base_ty), dims));
            }
            let size = self.parse_expr()?;
            self.eat_punct(Punct::RBracket)?;
            let mut dims = vec![Some(size)];
            self.parse_extra_array_dims(&mut dims)?;
            Ok(Expr::NewArray(Box::new(base_ty), dims))
        } else if self.is_punct(Punct::LParen) {
            let (class_name, type_args) = match base_ty {
                Type::Named(name) => (name, Vec::new()),
                Type::Generic(name, args) => (name, args),
                _ => {
                    return Err(SyntaxError::Parse(
                        "'new' on a primitive type requires array syntax 'new T[size]'".to_string(),
                        self.line(),
                self.col(),
                    ))
                }
            };
            self.bump();
            let args = self.parse_args()?;
            self.eat_punct(Punct::RParen)?;
            Ok(Expr::New(class_name, type_args, args))
        } else {
            Err(SyntaxError::Parse(
                "expected '(' or '[' after 'new <type>'".to_string(),
                self.line(),
                self.col(),
            ))
        }
    }

    /// Consumes any further `[size]`/`[]` groups after the first dimension
    /// of a `new T[...]` expression, appending one entry per group.
    fn parse_extra_array_dims(&mut self, dims: &mut Vec<Option<Expr>>) -> Result<(), SyntaxError> {
        while self.is_punct(Punct::LBracket) {
            self.bump();
            if self.is_punct(Punct::RBracket) {
                self.bump();
                dims.push(None);
            } else {
                let size = self.parse_expr()?;
                self.eat_punct(Punct::RBracket)?;
                dims.push(Some(size));
            }
        }
        Ok(())
    }

    /// Attempts `(params) => body` at the current position, restoring
    /// `self.pos` and returning `None` if it turns out not to be a closure
    /// (falls back to ordinary `(expr)` grouping in `parse_primary`) —
    /// there is no other lookahead that distinguishes `(int a, int b) => …`
    /// from a parenthesized expression without a param list that happens to
    /// start with a type-like identifier.
    fn try_parse_closure(&mut self) -> Result<Option<Expr>, SyntaxError> {
        let save = self.pos;
        match self.parse_closure() {
            Ok(closure) => Ok(Some(closure)),
            Err(_) => {
                self.pos = save;
                Ok(None)
            }
        }
    }

    /// specs.md § Anonymous Functions: `(params) => body`, with an optional
    /// `throws` clause and/or explicit return type before the body.
    ///
    /// Only a *primitive* return type (`int`/`float`/.../`void`) is
    /// supported, and only when immediately followed by `{` — e.g.
    /// `(int a, int b) => float { ... }`. A `Named` (class/interface) return
    /// type is genuinely ambiguous with the start of an expression-bodied
    /// closure (`(int a) => a` — is `a` a return type awaiting a body, or
    /// the body itself?); real implementations resolve this with deeper
    /// lookahead this parser doesn't attempt. Not implemented — a Named
    /// return type after `=>` is instead parsed as the closure's (invalid,
    /// will fail elsewhere) expression body.
    fn parse_closure(&mut self) -> Result<Expr, SyntaxError> {
        self.eat_punct(Punct::LParen)?;
        let mut params = Vec::new();
        while !self.is_punct(Punct::RParen) {
            // `const` on a closure parameter (specs.md's `(const string
            // text) => ...`) — compiler.md § Const parameters, E012, same
            // enforcement as an ordinary method parameter.
            let is_const = if self.is_keyword(Keyword::Const) {
                self.bump();
                true
            } else {
                false
            };
            let ty = self.parse_type()?;
            let name = self.eat_ident()?;
            let default = self.parse_param_default()?;
            // `ref` closure parameters aren't supported (see PLAN.md) —
            // `is_ref` is always `false` here.
            params.push(Param {
                name,
                ty,
                is_const,
                default,
                is_ref: false,
            });
            if self.is_punct(Punct::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.eat_punct(Punct::RParen)?;
        self.eat_punct(Punct::FatArrow)?;

        let throws = self.parse_throws_clause()?;
        let return_type = if self.peek_is_primitive_return_type() {
            Some(self.parse_type()?)
        } else {
            None
        };

        let body = if self.is_punct(Punct::LBrace) {
            ClosureBody::Block(self.parse_block()?)
        } else {
            ClosureBody::Expr(Box::new(self.parse_expr()?))
        };
        Ok(Expr::Closure {
            params,
            return_type,
            throws,
            body,
        })
    }

    /// Whether the current token could start a primitive/`void` return type
    /// immediately followed by `{` — the only return-type shape
    /// `parse_closure` accepts (see its doc comment).
    fn peek_is_primitive_return_type(&self) -> bool {
        let is_primitive = match &self.peek().kind {
            TokenKind::Keyword(Keyword::Void) => true,
            TokenKind::Ident(name) => {
                matches!(name.as_str(), "int" | "float" | "bool" | "byte" | "string")
            }
            _ => false,
        };
        is_primitive && matches!(self.peek_at(1), Some(TokenKind::Punct(Punct::LBrace)))
    }
}

/// Synthesizes `from()`/`tryFrom()` — specs.md § Enums methods. Both convert
/// a case *name* string to the matching case constant (`EnumName.CaseName`,
/// resolved by `nl-sema`/`nl-codegen` like any other static field access —
/// see `parse_enum_decl`'s doc comment); they differ only in what happens
/// when no case matches: `from` throws `IllegalArgumentException` (a
/// `RuntimeException`, so no `throws` clause is needed — compiler.md only
/// requires declaring/catching checked exceptions), `tryFrom` returns `null`.
fn enum_from_method(
    enum_name: &str,
    case_names: &[String],
    is_try: bool,
    decl_line: u32,
) -> MethodDecl {
    let mut body: Block = Vec::new();
    for case_name in case_names {
        let cond = Expr::Binary(
            BinOp::Eq,
            Box::new(Expr::Ident("value".to_string())),
            Box::new(Expr::StringLit(case_name.clone())),
        );
        let then_branch = vec![Stmt {
            kind: StmtKind::Return(Some(Expr::FieldAccess(
                Box::new(Expr::Ident(enum_name.to_string())),
                case_name.clone(),
            ))),
            line: decl_line,
        }];
        body.push(Stmt {
            kind: StmtKind::If {
                cond,
                then_branch,
                else_branch: None,
            },
            line: decl_line,
        });
    }
    let (name, return_type, fallback) = if is_try {
        (
            "tryFrom".to_string(),
            Type::Union(vec![Type::Named(enum_name.to_string()), Type::NullT]),
            Stmt {
                kind: StmtKind::Return(Some(Expr::NullLit)),
                line: decl_line,
            },
        )
    } else {
        (
            "from".to_string(),
            Type::Named(enum_name.to_string()),
            Stmt {
                kind: StmtKind::Throw(Expr::New(
                    "IllegalArgumentException".to_string(),
                    Vec::new(),
                    vec![Arg {
                        name: None,
                        is_ref: false,
                        value: Expr::StringLit(format!("unknown case for enum {enum_name}")),
                    }],
                )),
                line: decl_line,
            },
        )
    };
    body.push(fallback);
    MethodDecl {
        name,
        kind: MethodKind::Normal,
        visibility: Visibility::Public,
        visibility_explicit: true,
        is_static: true,
        is_const: false,
        is_abstract: false,
        is_final: false,
        is_nodiscard: false,
        return_type,
        params: vec![Param {
            name: "value".to_string(),
            ty: Type::StringT,
            is_const: false,
            default: None,
            is_ref: false,
        }],
        throws: Vec::new(),
        body,
        decl_line,
    }
}

fn to_lvalue(expr: Expr, line: u32, col: u32) -> Result<LValue, SyntaxError> {
    match expr {
        Expr::Ident(name) => Ok(LValue::Local(name)),
        Expr::FieldAccess(target, name) => Ok(LValue::Field(target, name)),
        Expr::Index(target, index) => Ok(LValue::Index(target, index)),
        _ => Err(SyntaxError::Parse(
            "invalid assignment target".to_string(),
            line,
            col,
        )),
    }
}
