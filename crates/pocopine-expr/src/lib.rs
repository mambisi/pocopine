//! Template expression parser + AST. Per RFC-012.
//!
//! This crate owns the **pure** half of pocopine's expression
//! pipeline — lexer, parser, AST. The runtime evaluator
//! (`pocopine_core::expr::evaluate`) and the proc-macro
//! validator (`pocopine_macros::for_plan`, RFC 054) both depend
//! on this crate.
//!
//! Splitting parser from evaluator lets the macro crate validate
//! template expressions at `cargo check` time without pulling in
//! `wasm-bindgen` / `web-sys` (host-incompatible).
//!
//! Hand-rolled recursive descent so:
//!
//! * the wasm bundle doesn't pay for a parser-combinator crate,
//! * errors carry structured spans a future `.poco` language server
//!   can consume directly (no second parse),
//! * the AST is trivially re-evaluable — the runtime caches it once
//!   per directive setup and walks it on every change.
//!
//! Grammar (operator precedence, lowest → highest):
//!
//! ```text
//! expr       := stmt_seq
//! stmt_seq   := stmt ( ';' stmt )*       // RFC-024
//! stmt       := assign | ternary
//! assign     := path '=' ternary         // RFC-024
//! ternary    := logic_or ( '?' expr ':' expr )?
//! logic_or   := logic_and ( '||' logic_and )*
//! logic_and  := equality  ( '&&' equality  )*
//! equality   := relation  ( ( '==' | '!=' ) relation )*
//! relation   := additive  ( ( '<=' | '<' | '>=' | '>' ) additive )*
//! additive   := unary     ( '+' unary )*
//! unary      := '!' unary | primary
//! primary    := literal | path | call | '(' expr ')'
//! call       := ident '(' ( ternary ( ',' ternary )* )? ')'   // RFC-024
//! literal    := string | number | 'true' | 'false' | 'null'
//! string     := '"' [^"]* '"' | "'" [^']* "'"
//! number     := /-?\d+(\.\d+)?/
//! path       := ident ( '.' ident )*
//! ident      := /[A-Za-z_$][A-Za-z0-9_$]*/
//! ```

use std::ops::Range;

/// Byte-range span into the original source string.
pub type Span = Range<usize>;

#[derive(Debug, Clone)]
pub struct Spanned<T> {
    pub value: T,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Literal),
    /// Dotted path segments, already split: `"foo.bar.baz"` →
    /// `vec!["foo", "bar", "baz"]`.
    Path(Vec<String>),
    Not(Box<Spanned<Expr>>),
    BinOp(BinOp, Box<Spanned<Expr>>, Box<Spanned<Expr>>),
    Ternary(Box<Spanned<Expr>>, Box<Spanned<Expr>>, Box<Spanned<Expr>>),
    /// `ident(arg, arg, ...)` — invokes a handler on the scope.
    /// RFC-024. Handlers resolve via `invoke_handler`; the name is
    /// a single identifier, not a path.
    Call(String, Vec<Spanned<Expr>>),
    /// `path = expr` — writes `expr` through the scope proxy's
    /// set trap at `path`. RFC-024. `path` is one or more dotted
    /// identifiers.
    Assign(Vec<String>, Box<Spanned<Expr>>),
    /// `a; b; c` — evaluated left-to-right; result is the last
    /// statement's value. RFC-024. Top-level form for `pp-on`
    /// values containing multiple statements.
    Seq(Vec<Spanned<Expr>>),
}

#[derive(Debug, Clone)]
pub enum Literal {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `+` — string concatenation when either operand is a string;
    /// numeric addition when both coerce to `f64`; empty string
    /// otherwise. Matches JS coercion closely enough for templates
    /// that compose IDs like `$id + '-title'`.
    Plus,
}

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
    pub hint: Option<String>,
}

// ─── lexer ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Bang,
    AndAnd,
    OrOr,
    EqEq,
    BangEq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Question,
    Colon,
    Dot,
    /// `,` — separates call arguments.
    Comma,
    /// `;` — separates statements in a `pp-on` value.
    Semi,
    /// Single `=` — assignment, per RFC-024. Distinct from
    /// `EqEq` (the equality check).
    Eq,
    Ident(String),
    StringLit(String),
    NumberLit(f64),
    True,
    False,
    Null,
    Eof,
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src: src.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self, offset: usize) -> Option<u8> {
        self.src.get(self.pos + offset).copied()
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek(0) {
            if c.is_ascii_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn next(&mut self) -> Result<(Tok, Span), ParseError> {
        self.skip_whitespace();
        let start = self.pos;
        let Some(c) = self.peek(0) else {
            return Ok((Tok::Eof, start..start));
        };
        let tok = match c {
            b'(' => {
                self.pos += 1;
                Tok::LParen
            }
            b')' => {
                self.pos += 1;
                Tok::RParen
            }
            b',' => {
                self.pos += 1;
                Tok::Comma
            }
            b';' => {
                self.pos += 1;
                Tok::Semi
            }
            b'+' => {
                self.pos += 1;
                Tok::Plus
            }
            // Arithmetic beyond `+` is not supported. Compute Rust-side
            // as a `#[computed]` field and bind by name. See
            // docs/guides/poco/04-expressions.md.
            b'*' | b'/' | b'%' => {
                return Err(self.err(
                    start..start + 1,
                    &format!(
                        "arithmetic operator `{}` is not supported in pine-expr",
                        c as char
                    ),
                    Some(
                        "compute Rust-side as a `#[computed]` field and bind by name — see docs/guides/poco/04-expressions.md",
                    ),
                ));
            }
            // Bare `-` (not part of a number literal) is unsupported
            // arithmetic. The number-literal case is the guarded arm
            // below this one; the match-first-wins rule means we
            // need the same guard inverted here so `-42` still
            // lexes as a number.
            b'-' if !matches!(self.peek(1), Some(b'0'..=b'9')) => {
                return Err(self.err(
                    start..start + 1,
                    "arithmetic subtraction is not supported in pine-expr",
                    Some(
                        "compute Rust-side as a `#[computed]` field and bind by name — see docs/guides/poco/04-expressions.md",
                    ),
                ));
            }
            b'?' => {
                self.pos += 1;
                if self.peek(0) == Some(b'?') {
                    return Err(self.err(
                        start..self.pos + 1,
                        "nullish coalescing `??` is not supported in pine-expr",
                        Some("use a ternary (`x == null ? fallback : x`) or compute Rust-side"),
                    ));
                }
                if self.peek(0) == Some(b'.') {
                    return Err(self.err(
                        start..self.pos + 1,
                        "optional chaining `?.` is not supported in pine-expr",
                        Some(
                            "compute Rust-side as a `#[computed]` field that handles the null case",
                        ),
                    ));
                }
                Tok::Question
            }
            b':' => {
                self.pos += 1;
                Tok::Colon
            }
            b'.' => {
                self.pos += 1;
                if self.peek(0) == Some(b'.') {
                    return Err(self.err(
                        start..self.pos + 1,
                        "spread `...` is not supported in pine-expr",
                        Some("pass arguments explicitly or compute Rust-side"),
                    ));
                }
                Tok::Dot
            }
            b'!' => {
                self.pos += 1;
                if self.peek(0) == Some(b'=') {
                    self.pos += 1;
                    if self.peek(0) == Some(b'=') {
                        return Err(self.err(
                            start..self.pos + 1,
                            "`!==` is not supported in pine-expr",
                            Some("use `!=` (pine-expr uses Rust-style equality)"),
                        ));
                    }
                    Tok::BangEq
                } else {
                    Tok::Bang
                }
            }
            b'=' => {
                // `==` wins over `=` — peek the next byte before
                // committing. Single `=` is assignment (RFC-024);
                // `==` is equality (RFC-012). `===`, `!==`, and `=>`
                // are rejected with directive messages so JS muscle
                // memory fails loud at compile time.
                if self.peek(1) == Some(b'=') {
                    if self.peek(2) == Some(b'=') {
                        return Err(self.err(
                            start..self.pos + 3,
                            "`===` is not supported in pine-expr",
                            Some("use `==` (pine-expr uses Rust-style equality)"),
                        ));
                    }
                    self.pos += 2;
                    Tok::EqEq
                } else if self.peek(1) == Some(b'>') {
                    return Err(self.err(
                        start..self.pos + 2,
                        "arrow functions are not supported in pine-expr",
                        Some(
                            "define a handler method on the component — see docs/guides/poco/04-expressions.md",
                        ),
                    ));
                } else {
                    self.pos += 1;
                    Tok::Eq
                }
            }
            b'&' => {
                if self.peek(1) != Some(b'&') {
                    return Err(self.err(start..start + 1, "expected `&&`", None));
                }
                self.pos += 2;
                Tok::AndAnd
            }
            b'|' => {
                if self.peek(1) != Some(b'|') {
                    return Err(self.err(start..start + 1, "expected `||`", None));
                }
                self.pos += 2;
                Tok::OrOr
            }
            b'<' => {
                self.pos += 1;
                if self.peek(0) == Some(b'=') {
                    self.pos += 1;
                    Tok::Le
                } else {
                    Tok::Lt
                }
            }
            b'>' => {
                self.pos += 1;
                if self.peek(0) == Some(b'=') {
                    self.pos += 1;
                    Tok::Ge
                } else {
                    Tok::Gt
                }
            }
            b'"' | b'\'' => self.string(c, start)?,
            b'0'..=b'9' => self.number(start)?,
            b'-' if matches!(self.peek(1), Some(b'0'..=b'9')) => self.number(start)?,
            c if is_ident_start(c) => self.ident(start),
            _ => {
                return Err(self.err(
                    start..start + 1,
                    &format!("unexpected character {:?}", c as char),
                    None,
                ));
            }
        };
        Ok((tok, start..self.pos))
    }

    fn string(&mut self, quote: u8, start: usize) -> Result<Tok, ParseError> {
        self.pos += 1; // opening quote
        let content_start = self.pos;
        while let Some(c) = self.peek(0) {
            if c == quote {
                let s = std::str::from_utf8(&self.src[content_start..self.pos])
                    .unwrap_or_default()
                    .to_string();
                self.pos += 1; // closing quote
                return Ok(Tok::StringLit(s));
            }
            self.pos += 1;
        }
        Err(self.err(start..self.pos, "unterminated string literal", None))
    }

    fn number(&mut self, start: usize) -> Result<Tok, ParseError> {
        if self.peek(0) == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(0), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.peek(0) == Some(b'.') && matches!(self.peek(1), Some(b'0'..=b'9')) {
            self.pos += 1; // dot
            while matches!(self.peek(0), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let s = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or_default();
        let n = s.parse::<f64>().map_err(|_| ParseError {
            message: format!("invalid number literal {s:?}"),
            span: start..self.pos,
            hint: None,
        })?;
        Ok(Tok::NumberLit(n))
    }

    fn ident(&mut self, start: usize) -> Tok {
        while matches!(self.peek(0), Some(c) if is_ident_continue(c)) {
            self.pos += 1;
        }
        let s = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or_default();
        match s {
            "true" => Tok::True,
            "false" => Tok::False,
            "null" => Tok::Null,
            _ => Tok::Ident(s.to_string()),
        }
    }

    fn err(&self, span: Span, msg: &str, hint: Option<&str>) -> ParseError {
        ParseError {
            message: msg.to_string(),
            span,
            hint: hint.map(|s| s.to_string()),
        }
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$'
}
fn is_ident_continue(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

// ─── parser ───────────────────────────────────────────────────────

struct Parser {
    toks: Vec<(Tok, Span)>,
    pos: usize,
}

impl Parser {
    fn new(src: &str) -> Result<Self, ParseError> {
        let mut lex = Lexer::new(src);
        let mut toks = Vec::new();
        loop {
            let (tok, span) = lex.next()?;
            let is_eof = tok == Tok::Eof;
            toks.push((tok, span));
            if is_eof {
                break;
            }
        }
        Ok(Self { toks, pos: 0 })
    }

    fn peek(&self) -> &(Tok, Span) {
        &self.toks[self.pos]
    }

    fn eat(&mut self, want: &Tok) -> bool {
        if std::mem::discriminant(&self.peek().0) == std::mem::discriminant(want) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, want: &Tok, msg: &str) -> Result<Span, ParseError> {
        let (tok, span) = self.peek().clone();
        if std::mem::discriminant(&tok) == std::mem::discriminant(want) {
            self.pos += 1;
            Ok(span)
        } else {
            Err(ParseError {
                message: msg.to_string(),
                span,
                hint: None,
            })
        }
    }

    fn parse_expr(&mut self) -> Result<Spanned<Expr>, ParseError> {
        let e = self.parse_stmt_seq()?;
        if self.peek().0 != Tok::Eof {
            let span = self.peek().1.clone();
            return Err(ParseError {
                message: "unexpected trailing tokens".to_string(),
                span,
                hint: None,
            });
        }
        Ok(e)
    }

    /// One or more statements separated by `;`. A single-statement
    /// value (the overwhelmingly common case) returns the inner
    /// AST directly; two or more fold into `Expr::Seq`. Trailing
    /// `;` is tolerated.
    fn parse_stmt_seq(&mut self) -> Result<Spanned<Expr>, ParseError> {
        let first = self.parse_stmt()?;
        if !matches!(self.peek().0, Tok::Semi) {
            return Ok(first);
        }
        let mut stmts = vec![first];
        while self.eat(&Tok::Semi) {
            if matches!(self.peek().0, Tok::Eof) {
                break; // trailing `;`
            }
            stmts.push(self.parse_stmt()?);
        }
        let start = stmts.first().map(|s| s.span.start).unwrap_or(0);
        let end = stmts.last().map(|s| s.span.end).unwrap_or(0);
        Ok(Spanned {
            value: Expr::Seq(stmts),
            span: start..end,
        })
    }

    /// Either `path = expr` (assignment, RFC-024) or a plain
    /// expression. Assignment is recognised by a look-ahead: if the
    /// token after a `Path` primary is `=` (single `=`, not `==`),
    /// treat the path as an l-value and consume the RHS.
    fn parse_stmt(&mut self) -> Result<Spanned<Expr>, ParseError> {
        let lhs = self.parse_expr_top()?;
        if !matches!(self.peek().0, Tok::Eq) {
            return Ok(lhs);
        }
        // Assignment. LHS must be a plain path.
        let Expr::Path(segments) = lhs.value else {
            let span = self.peek().1.clone();
            return Err(ParseError {
                message: "left side of `=` must be a path".to_string(),
                span,
                hint: Some(
                    "only dotted identifiers like `foo` or `foo.bar` are assignable".to_string(),
                ),
            });
        };
        self.pos += 1; // consume `=`
        let rhs = self.parse_expr_top()?;
        let span = lhs.span.start..rhs.span.end;
        Ok(Spanned {
            value: Expr::Assign(segments, Box::new(rhs)),
            span,
        })
    }

    fn parse_expr_top(&mut self) -> Result<Spanned<Expr>, ParseError> {
        self.parse_ternary()
    }

    fn parse_ternary(&mut self) -> Result<Spanned<Expr>, ParseError> {
        let cond = self.parse_or()?;
        if self.eat(&Tok::Question) {
            let then_e = self.parse_expr_top()?;
            self.expect(&Tok::Colon, "expected `:` in ternary expression")?;
            let else_e = self.parse_expr_top()?;
            let span = cond.span.start..else_e.span.end;
            return Ok(Spanned {
                value: Expr::Ternary(Box::new(cond), Box::new(then_e), Box::new(else_e)),
                span,
            });
        }
        Ok(cond)
    }

    fn parse_or(&mut self) -> Result<Spanned<Expr>, ParseError> {
        let mut lhs = self.parse_and()?;
        while self.eat(&Tok::OrOr) {
            let rhs = self.parse_and()?;
            let span = lhs.span.start..rhs.span.end;
            lhs = Spanned {
                value: Expr::BinOp(BinOp::Or, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Spanned<Expr>, ParseError> {
        let mut lhs = self.parse_equality()?;
        while self.eat(&Tok::AndAnd) {
            let rhs = self.parse_equality()?;
            let span = lhs.span.start..rhs.span.end;
            lhs = Spanned {
                value: Expr::BinOp(BinOp::And, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_equality(&mut self) -> Result<Spanned<Expr>, ParseError> {
        let mut lhs = self.parse_relation()?;
        loop {
            let op = match self.peek().0 {
                Tok::EqEq => BinOp::Eq,
                Tok::BangEq => BinOp::Ne,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_relation()?;
            let span = lhs.span.start..rhs.span.end;
            lhs = Spanned {
                value: Expr::BinOp(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_relation(&mut self) -> Result<Spanned<Expr>, ParseError> {
        let mut lhs = self.parse_additive()?;
        loop {
            let op = match self.peek().0 {
                Tok::Le => BinOp::Le,
                Tok::Lt => BinOp::Lt,
                Tok::Ge => BinOp::Ge,
                Tok::Gt => BinOp::Gt,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_additive()?;
            let span = lhs.span.start..rhs.span.end;
            lhs = Spanned {
                value: Expr::BinOp(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> Result<Spanned<Expr>, ParseError> {
        let mut lhs = self.parse_unary()?;
        while self.eat(&Tok::Plus) {
            let rhs = self.parse_unary()?;
            let span = lhs.span.start..rhs.span.end;
            lhs = Spanned {
                value: Expr::BinOp(BinOp::Plus, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Spanned<Expr>, ParseError> {
        let (tok, span) = self.peek().clone();
        if tok == Tok::Bang {
            self.pos += 1;
            let inner = self.parse_unary()?;
            let outer = span.start..inner.span.end;
            return Ok(Spanned {
                value: Expr::Not(Box::new(inner)),
                span: outer,
            });
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Spanned<Expr>, ParseError> {
        let (tok, span) = self.peek().clone();
        match tok {
            Tok::LParen => {
                self.pos += 1;
                let e = self.parse_expr_top()?;
                self.expect(&Tok::RParen, "expected `)`")?;
                Ok(e)
            }
            Tok::StringLit(s) => {
                self.pos += 1;
                Ok(Spanned {
                    value: Expr::Literal(Literal::String(s)),
                    span,
                })
            }
            Tok::NumberLit(n) => {
                self.pos += 1;
                Ok(Spanned {
                    value: Expr::Literal(Literal::Number(n)),
                    span,
                })
            }
            Tok::True => {
                self.pos += 1;
                Ok(Spanned {
                    value: Expr::Literal(Literal::Bool(true)),
                    span,
                })
            }
            Tok::False => {
                self.pos += 1;
                Ok(Spanned {
                    value: Expr::Literal(Literal::Bool(false)),
                    span,
                })
            }
            Tok::Null => {
                self.pos += 1;
                Ok(Spanned {
                    value: Expr::Literal(Literal::Null),
                    span,
                })
            }
            Tok::Ident(first) => {
                self.pos += 1;
                let start = span.start;
                // `ident(` → call. Only plain identifiers can be
                // called; `foo.bar(…)` is not supported in v0
                // (handlers are scope methods, not properties on
                // sub-objects).
                if matches!(self.peek().0, Tok::LParen) {
                    self.pos += 1; // consume `(`
                    let mut args = Vec::new();
                    if !matches!(self.peek().0, Tok::RParen) {
                        loop {
                            args.push(self.parse_expr_top()?);
                            if self.eat(&Tok::Comma) {
                                continue;
                            }
                            break;
                        }
                    }
                    let end = self.peek().1.end;
                    self.expect(&Tok::RParen, "expected `)` closing call arguments")?;
                    return Ok(Spanned {
                        value: Expr::Call(first, args),
                        span: start..end,
                    });
                }
                let mut segments = vec![first];
                let mut end = span.end;
                while self.eat(&Tok::Dot) {
                    let (tok, s) = self.peek().clone();
                    match tok {
                        Tok::Ident(seg) => {
                            self.pos += 1;
                            end = s.end;
                            segments.push(seg);
                        }
                        _ => {
                            return Err(ParseError {
                                message: "expected identifier after `.`".to_string(),
                                span: s,
                                hint: None,
                            });
                        }
                    }
                }
                // `obj.method(…)` — only plain identifiers can be
                // called. A path followed by `(` is the JS muscle
                // memory we want to teach against: define a plain
                // identifier handler that takes the object as an
                // argument instead.
                if matches!(self.peek().0, Tok::LParen) && segments.len() > 1 {
                    return Err(ParseError {
                        message: "method calls on objects are not supported in pine-expr"
                            .to_string(),
                        span: self.peek().1.clone(),
                        hint: Some(
                            "define a plain identifier handler, or a `#[computed]` field, that takes the object as an argument — see docs/guides/poco/04-expressions.md"
                                .to_string(),
                        ),
                    });
                }
                Ok(Spanned {
                    value: Expr::Path(segments),
                    span: start..end,
                })
            }
            _ => Err(ParseError {
                message: "expected expression".to_string(),
                span,
                hint: None,
            }),
        }
    }
}

/// Parse `src` into a reusable AST. Returns structured errors with
/// byte spans so a future `.poco` LSP can surface them without a
/// second parse.
pub fn parse(src: &str) -> Result<Spanned<Expr>, ParseError> {
    if src.trim().is_empty() {
        return Err(ParseError {
            message: "empty expression".to_string(),
            span: 0..src.len(),
            hint: None,
        });
    }
    let mut p = Parser::new(src)?;
    p.parse_expr()
}

// ─── tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(src: &str) -> Spanned<Expr> {
        parse(src).unwrap_or_else(|e| panic!("parse failed for {src:?}: {:?}", e))
    }

    fn parse_err(src: &str) {
        assert!(parse(src).is_err(), "expected parse error for {src:?}");
    }

    #[test]
    fn literals() {
        matches!(parse_ok("true").value, Expr::Literal(Literal::Bool(true)));
        matches!(parse_ok("false").value, Expr::Literal(Literal::Bool(false)));
        matches!(parse_ok("null").value, Expr::Literal(Literal::Null));
        matches!(parse_ok("42").value, Expr::Literal(Literal::Number(_)));
        matches!(parse_ok("\"hi\"").value, Expr::Literal(Literal::String(_)));
    }

    #[test]
    fn path_segments() {
        let e = parse_ok("foo.bar.baz");
        match e.value {
            Expr::Path(segs) => assert_eq!(segs, vec!["foo", "bar", "baz"]),
            other => panic!("expected Path, got {other:?}"),
        }
    }

    #[test]
    fn precedence() {
        // `a && b || c` → `(a && b) || c`
        let e = parse_ok("a && b || c");
        match e.value {
            Expr::BinOp(BinOp::Or, lhs, rhs) => {
                assert!(matches!(lhs.value, Expr::BinOp(BinOp::And, _, _)));
                assert!(matches!(rhs.value, Expr::Path(_)));
            }
            other => panic!("expected OR at top, got {other:?}"),
        }
    }

    #[test]
    fn not_prefix_and_comparison() {
        let e = parse_ok("!(a == b)");
        assert!(matches!(e.value, Expr::Not(_)));
    }

    #[test]
    fn ternary_right_associative() {
        // a ? b : c ? d : e  ==  a ? b : (c ? d : e)
        let e = parse_ok("a ? b : c ? d : e");
        match e.value {
            Expr::Ternary(_, _, else_e) => {
                assert!(matches!(else_e.value, Expr::Ternary(_, _, _)));
            }
            other => panic!("expected Ternary, got {other:?}"),
        }
    }

    #[test]
    fn string_literals_both_quotes() {
        matches!(parse_ok("'hello'").value, Expr::Literal(Literal::String(_)));
        matches!(
            parse_ok("\"world\"").value,
            Expr::Literal(Literal::String(_))
        );
    }

    #[test]
    fn errors_carry_spans() {
        match parse("a ||") {
            Err(ParseError { span, .. }) => assert!(span.start >= 3),
            Ok(_) => panic!("expected error"),
        }
    }

    // RFC-024: `a = b` is now a valid assignment statement.
    #[test]
    fn parses_assignment() {
        let e = parse_ok("open = true");
        match e.value {
            Expr::Assign(ref path, ref rhs) => {
                assert_eq!(path, &vec!["open".to_string()]);
                assert!(matches!(rhs.value, Expr::Literal(Literal::Bool(true))));
            }
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn parses_call_zero_args() {
        let e = parse_ok("close()");
        match e.value {
            Expr::Call(name, args) => {
                assert_eq!(name, "close");
                assert!(args.is_empty());
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn parses_call_with_args() {
        let e = parse_ok("select(item.value, 42)");
        match e.value {
            Expr::Call(name, args) => {
                assert_eq!(name, "select");
                assert_eq!(args.len(), 2);
                assert!(matches!(args[0].value, Expr::Path(_)));
                assert!(matches!(args[1].value, Expr::Literal(Literal::Number(_))));
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn parses_statement_sequence() {
        let e = parse_ok("copy($event); close()");
        match e.value {
            Expr::Seq(ref stmts) => {
                assert_eq!(stmts.len(), 2);
                assert!(matches!(stmts[0].value, Expr::Call(_, _)));
                assert!(matches!(stmts[1].value, Expr::Call(_, _)));
            }
            other => panic!("expected Seq, got {other:?}"),
        }
    }

    #[test]
    fn assignment_rhs_can_be_expression() {
        // `open = !open` parses with `!open` as the RHS.
        let e = parse_ok("open = !open");
        match e.value {
            Expr::Assign(path, rhs) => {
                assert_eq!(path, vec!["open".to_string()]);
                assert!(matches!(rhs.value, Expr::Not(_)));
            }
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn rejects_trailing_garbage() {
        parse_err("a b");
    }

    // Associativity + nesting coverage — the grammar commits to
    // left-associative binops and right-associative ternary, matching
    // JS / Vue / React semantics.

    #[test]
    fn and_is_left_associative() {
        // `a && b && c` → `(a && b) && c`
        let e = parse_ok("a && b && c");
        match e.value {
            Expr::BinOp(BinOp::And, lhs, rhs) => {
                assert!(
                    matches!(lhs.value, Expr::BinOp(BinOp::And, _, _)),
                    "left arm should be another AND",
                );
                assert!(
                    matches!(rhs.value, Expr::Path(_)),
                    "right arm should be a leaf path",
                );
            }
            other => panic!("expected top-level AND, got {other:?}"),
        }
    }

    #[test]
    fn or_is_left_associative() {
        let e = parse_ok("a || b || c");
        match e.value {
            Expr::BinOp(BinOp::Or, lhs, rhs) => {
                assert!(matches!(lhs.value, Expr::BinOp(BinOp::Or, _, _)));
                assert!(matches!(rhs.value, Expr::Path(_)));
            }
            other => panic!("expected top-level OR, got {other:?}"),
        }
    }

    #[test]
    fn equality_is_left_associative() {
        // `a == b == c` → `(a == b) == c`. Unusual but legal.
        let e = parse_ok("a == b == c");
        match e.value {
            Expr::BinOp(BinOp::Eq, lhs, rhs) => {
                assert!(matches!(lhs.value, Expr::BinOp(BinOp::Eq, _, _)));
                assert!(matches!(rhs.value, Expr::Path(_)));
            }
            other => panic!("expected EQ at top, got {other:?}"),
        }
    }

    #[test]
    fn not_or_and_mixed_precedence() {
        // `!a || b && c` → `(!a) || (b && c)` —
        // `!` binds tightest, `&&` tighter than `||`.
        let e = parse_ok("!a || b && c");
        match e.value {
            Expr::BinOp(BinOp::Or, lhs, rhs) => {
                assert!(matches!(lhs.value, Expr::Not(_)), "left is `!a`");
                assert!(
                    matches!(rhs.value, Expr::BinOp(BinOp::And, _, _)),
                    "right is `b && c`",
                );
            }
            other => panic!("expected OR at top, got {other:?}"),
        }
    }

    #[test]
    fn relation_tighter_than_equality_tighter_than_and() {
        // `a < b == c && d` → `((a < b) == c) && d`
        let e = parse_ok("a < b == c && d");
        match e.value {
            Expr::BinOp(BinOp::And, lhs, rhs) => {
                match lhs.value {
                    Expr::BinOp(BinOp::Eq, eq_l, _) => {
                        assert!(matches!(eq_l.value, Expr::BinOp(BinOp::Lt, _, _)));
                    }
                    other => panic!("expected EQ inside AND's left, got {other:?}"),
                }
                assert!(matches!(rhs.value, Expr::Path(_)));
            }
            other => panic!("expected AND at top, got {other:?}"),
        }
    }

    #[test]
    fn parens_override_precedence() {
        // `a && (b || c)` → top is AND, not OR as it would be without parens.
        let e = parse_ok("a && (b || c)");
        match e.value {
            Expr::BinOp(BinOp::And, _, rhs) => {
                assert!(matches!(rhs.value, Expr::BinOp(BinOp::Or, _, _)));
            }
            other => panic!("expected AND at top after parens, got {other:?}"),
        }
    }

    #[test]
    fn deeply_nested_parens() {
        // `((a || b) && (c || d))` — two-level paren nest resolves
        // correctly; top is AND.
        let e = parse_ok("((a || b) && (c || d))");
        match e.value {
            Expr::BinOp(BinOp::And, lhs, rhs) => {
                assert!(matches!(lhs.value, Expr::BinOp(BinOp::Or, _, _)));
                assert!(matches!(rhs.value, Expr::BinOp(BinOp::Or, _, _)));
            }
            other => panic!("expected AND between two OR groups, got {other:?}"),
        }
    }

    #[test]
    fn nested_path_many_segments() {
        let e = parse_ok("a.b.c.d.e");
        match e.value {
            Expr::Path(segs) => assert_eq!(segs, vec!["a", "b", "c", "d", "e"]),
            other => panic!("expected Path, got {other:?}"),
        }
    }

    #[test]
    fn ternary_with_complex_condition() {
        // `a && b ? c : d` — `&&` binds tighter than `?:`, so
        // condition is `(a && b)`.
        let e = parse_ok("a && b ? c : d");
        match e.value {
            Expr::Ternary(cond, _, _) => {
                assert!(matches!(cond.value, Expr::BinOp(BinOp::And, _, _)));
            }
            other => panic!("expected Ternary, got {other:?}"),
        }
    }

    #[test]
    fn ternary_in_ternary_branches() {
        // `a ? (b ? c : d) : (e ? f : g)` — nested ternary in both arms.
        let e = parse_ok("a ? (b ? c : d) : (e ? f : g)");
        match e.value {
            Expr::Ternary(_, then_e, else_e) => {
                assert!(matches!(then_e.value, Expr::Ternary(_, _, _)));
                assert!(matches!(else_e.value, Expr::Ternary(_, _, _)));
            }
            other => panic!("expected Ternary, got {other:?}"),
        }
    }

    #[test]
    fn unary_not_stacks() {
        // `!!a` → Not(Not(a)). No special-case collapse; the
        // evaluator computes `true` for truthy `a`.
        let e = parse_ok("!!a");
        match e.value {
            Expr::Not(inner) => assert!(matches!(inner.value, Expr::Not(_))),
            other => panic!("expected Not(Not), got {other:?}"),
        }
    }

    #[test]
    fn comparison_with_string_literal() {
        let e = parse_ok("role == 'admin' || role == \"editor\"");
        assert!(matches!(e.value, Expr::BinOp(BinOp::Or, _, _)));
    }

    // ─── RFC-018 — `+` operator ─────────────────────────────────

    #[test]
    fn plus_chains_left_to_right() {
        let e = parse_ok("a + b + c");
        // (a + b) + c
        match e.value {
            Expr::BinOp(BinOp::Plus, lhs, rhs) => {
                assert!(matches!(lhs.value, Expr::BinOp(BinOp::Plus, _, _)));
                assert!(matches!(rhs.value, Expr::Path(_)));
            }
            other => panic!("expected BinOp Plus, got {other:?}"),
        }
    }

    #[test]
    fn plus_binds_tighter_than_comparison() {
        let e = parse_ok("a + b == 'foo-bar'");
        match e.value {
            Expr::BinOp(BinOp::Eq, lhs, _) => {
                assert!(matches!(lhs.value, Expr::BinOp(BinOp::Plus, _, _)));
            }
            other => panic!("expected equality, got {other:?}"),
        }
    }

    #[test]
    fn plus_binds_looser_than_unary_not() {
        // `!a + !b` should parse as `(!a) + (!b)` — i.e. `+` is
        // outside, `!` is inside.
        let e = parse_ok("!a + !b");
        match e.value {
            Expr::BinOp(BinOp::Plus, lhs, rhs) => {
                assert!(matches!(lhs.value, Expr::Not(_)));
                assert!(matches!(rhs.value, Expr::Not(_)));
            }
            other => panic!("expected plus at root, got {other:?}"),
        }
    }

    #[test]
    fn plus_with_string_literal_parses() {
        let e = parse_ok("$id + '-title'");
        assert!(matches!(e.value, Expr::BinOp(BinOp::Plus, _, _)));
    }

    // ── Teaching errors: JS muscle memory rejected at compile time ──
    //
    // Every case below was already a parse error with a less helpful
    // message. These tests pin the directive wording so future edits
    // don't regress the diagnostic.

    fn assert_err_says(src: &str, needle: &str) {
        let err = parse(src).expect_err(&format!("expected parse error for {src:?}"));
        assert!(
            err.message.contains(needle) || err.hint.as_deref().unwrap_or("").contains(needle),
            "error for {src:?} should mention {needle:?}; got message={:?} hint={:?}",
            err.message,
            err.hint,
        );
    }

    #[test]
    fn reject_arithmetic_star() {
        assert_err_says("progress * 100", "arithmetic operator");
        assert_err_says("progress * 100", "#[computed]");
    }

    #[test]
    fn reject_arithmetic_slash() {
        assert_err_says("total / count", "arithmetic operator");
    }

    #[test]
    fn reject_arithmetic_percent() {
        assert_err_says("idx % 2", "arithmetic operator");
    }

    #[test]
    fn reject_arithmetic_minus() {
        assert_err_says("count - 1", "subtraction");
        assert_err_says("count - 1", "#[computed]");
    }

    #[test]
    fn reject_triple_equals() {
        assert_err_says("status === 'queued'", "`===`");
        assert_err_says("status === 'queued'", "use `==`");
    }

    #[test]
    fn reject_strict_inequality() {
        assert_err_says("status !== 'queued'", "`!==`");
        assert_err_says("status !== 'queued'", "use `!=`");
    }

    #[test]
    fn reject_arrow_function() {
        assert_err_says("x => x", "arrow functions");
        assert_err_says("x => x", "handler method");
    }

    #[test]
    fn reject_nullish_coalescing() {
        assert_err_says("a ?? b", "nullish coalescing");
    }

    #[test]
    fn reject_optional_chaining() {
        assert_err_says("user?.name", "optional chaining");
    }

    #[test]
    fn reject_spread() {
        assert_err_says("...rest", "spread");
    }

    #[test]
    fn reject_method_call_on_path() {
        // `files.filter(...)` — a path of length >= 2 followed by `(`
        // is the JS muscle-memory case we want to flag.
        assert_err_says("files.filter(f)", "method calls on objects");
        assert_err_says("files.filter(f)", "#[computed]");
    }

    #[test]
    fn negative_number_literal_still_parses() {
        // The `-` rejection must not break `-42` as a number literal.
        let e = parse_ok("-42");
        assert!(matches!(
            e.value,
            Expr::Literal(Literal::Number(n)) if n == -42.0
        ));
    }

    #[test]
    fn plain_identifier_call_still_parses() {
        // `obj.method(...)` is rejected, but `method(obj)` is the
        // canonical replacement and must still parse.
        let e = parse_ok("filter_done(files)");
        assert!(matches!(e.value, Expr::Call(_, _)));
    }
}
