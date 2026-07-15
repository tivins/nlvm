use crate::error::SyntaxError;
use crate::token::{Keyword, Punct, Token, TokenKind};

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: u32,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self {
            src: src.as_bytes(),
            pos: 0,
            line: 1,
        }
    }

    pub fn tokenize(mut self) -> Result<Vec<Token>, SyntaxError> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token()?;
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        Ok(tokens)
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.src.get(self.pos + offset).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') => {
                    self.bump();
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    self.bump();
                    self.bump();
                    while let Some(c) = self.peek() {
                        if c == b'*' && self.peek_at(1) == Some(b'/') {
                            self.bump();
                            self.bump();
                            break;
                        }
                        self.bump();
                    }
                }
                _ => break,
            }
        }
    }

    fn next_token(&mut self) -> Result<Token, SyntaxError> {
        self.skip_trivia();
        let line = self.line;
        let Some(c) = self.peek() else {
            return Ok(Token {
                kind: TokenKind::Eof,
                line,
            });
        };

        if is_ident_start(c) {
            return Ok(Token {
                kind: self.lex_ident_or_keyword(),
                line,
            });
        }
        if c.is_ascii_digit() {
            return Ok(Token {
                kind: self.lex_number()?,
                line,
            });
        }
        if c == b'"' {
            return Ok(Token {
                kind: self.lex_string()?,
                line,
            });
        }

        self.lex_punct(line)
    }

    fn lex_ident_or_keyword(&mut self) -> TokenKind {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if is_ident_continue(c) {
                self.bump();
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.pos])
            .unwrap()
            .to_string();
        match Keyword::lookup(&text) {
            Some(kw) => TokenKind::Keyword(kw),
            None => TokenKind::Ident(text),
        }
    }

    fn lex_number(&mut self) -> Result<TokenKind, SyntaxError> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.bump();
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') && matches!(self.peek_at(1), Some(c) if c.is_ascii_digit()) {
            is_float = true;
            self.bump();
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap();
        if is_float {
            let v: f64 = text
                .parse()
                .map_err(|_| SyntaxError::Lex(format!("invalid float literal '{text}'"), self.line))?;
            Ok(TokenKind::FloatLiteral(v))
        } else {
            let v: i64 = text
                .parse()
                .map_err(|_| SyntaxError::Lex(format!("invalid int literal '{text}'"), self.line))?;
            Ok(TokenKind::IntLiteral(v))
        }
    }

    fn lex_string(&mut self) -> Result<TokenKind, SyntaxError> {
        self.bump(); // opening quote
        let mut s = String::new();
        loop {
            match self.bump() {
                Some(b'"') => break,
                Some(b'\\') => match self.bump() {
                    Some(b'n') => s.push('\n'),
                    Some(b't') => s.push('\t'),
                    Some(b'"') => s.push('"'),
                    Some(b'\\') => s.push('\\'),
                    Some(other) => s.push(other as char),
                    None => return Err(SyntaxError::Lex("unterminated string".into(), self.line)),
                },
                Some(c) => s.push(c as char),
                None => return Err(SyntaxError::Lex("unterminated string".into(), self.line)),
            }
        }
        Ok(TokenKind::StringLiteral(s))
    }

    fn lex_punct(&mut self, line: u32) -> Result<Token, SyntaxError> {
        macro_rules! two {
            ($p:expr) => {{
                self.bump();
                self.bump();
                Ok(Token { kind: TokenKind::Punct($p), line })
            }};
        }
        macro_rules! one {
            ($p:expr) => {{
                self.bump();
                Ok(Token { kind: TokenKind::Punct($p), line })
            }};
        }

        let c = self.peek().unwrap();
        let c1 = self.peek_at(1);
        match (c, c1) {
            (b'(', _) => one!(Punct::LParen),
            (b')', _) => one!(Punct::RParen),
            (b'{', _) => one!(Punct::LBrace),
            (b'}', _) => one!(Punct::RBrace),
            (b'[', _) => one!(Punct::LBracket),
            (b']', _) => one!(Punct::RBracket),
            (b';', _) => one!(Punct::Semi),
            (b',', _) => one!(Punct::Comma),
            (b'.', _) => one!(Punct::Dot),
            (b':', _) => one!(Punct::Colon),
            (b'+', Some(b'+')) => two!(Punct::PlusPlus),
            (b'+', _) => one!(Punct::Plus),
            (b'-', Some(b'-')) => two!(Punct::MinusMinus),
            (b'-', Some(b'>')) => two!(Punct::Arrow),
            (b'-', _) => one!(Punct::Minus),
            (b'*', _) => one!(Punct::Star),
            (b'/', _) => one!(Punct::Slash),
            (b'%', _) => one!(Punct::Percent),
            (b'=', Some(b'=')) => two!(Punct::EqEq),
            (b'=', _) => one!(Punct::Assign),
            (b'!', Some(b'=')) => two!(Punct::NotEq),
            (b'!', _) => one!(Punct::Not),
            (b'<', Some(b'=')) if self.peek_at(2) == Some(b'>') => {
                self.bump();
                self.bump();
                self.bump();
                Ok(Token { kind: TokenKind::Punct(Punct::Spaceship), line })
            }
            (b'<', Some(b'=')) => two!(Punct::Le),
            (b'<', _) => one!(Punct::Lt),
            (b'>', Some(b'=')) => two!(Punct::Ge),
            (b'>', _) => one!(Punct::Gt),
            (b'&', Some(b'&')) => two!(Punct::AndAnd),
            (b'&', _) => one!(Punct::Amp),
            (b'|', Some(b'|')) => two!(Punct::OrOr),
            (b'|', _) => one!(Punct::Pipe),
            (b'?', Some(b'?')) => two!(Punct::QuestionQuestion),
            (b'?', Some(b':')) => two!(Punct::QuestionColon),
            (b'?', _) => one!(Punct::Question),
            (other, _) => Err(SyntaxError::Lex(
                format!("unexpected character '{}'", other as char),
                line,
            )),
        }
    }
}

fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic() || c >= 0x80
}

fn is_ident_continue(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}
