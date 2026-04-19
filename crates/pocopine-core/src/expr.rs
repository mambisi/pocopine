//! Template expression parser + evaluator. Per RFC-012.
//!
//! Used by `pp-show`, `pp-if`, `pp-text`, `pp-html`, and
//! `pp-bind:*` directive values. Hand-rolled recursive descent so:
//!
//! * the wasm bundle doesn't pay for a parser-combinator crate,
//! * errors carry structured spans a future `.poco` language server
//!   can consume directly (no second parse),
//! * the AST is trivially re-evaluable — we cache it once per
//!   directive setup and walk it on every change.
//!
//! Grammar (operator precedence, lowest → highest):
//!
//! ```text
//! expr       := ternary
//! ternary    := logic_or ( '?' expr ':' expr )?
//! logic_or   := logic_and ( '||' logic_and )*
//! logic_and  := equality  ( '&&' equality  )*
//! equality   := relation  ( ( '==' | '!=' ) relation )*
//! relation   := unary     ( ( '<=' | '<' | '>=' | '>' ) unary )*
//! unary      := '!' unary | primary
//! primary    := literal | path | '(' expr ')'
//! literal    := string | number | 'true' | 'false' | 'null'
//! string     := '"' [^"]* '"' | "'" [^']* "'"
//! number     := /-?\d+(\.\d+)?/
//! path       := ident ( '.' ident )*
//! ident      := /[A-Za-z_$][A-Za-z0-9_$]*/
//! ```

use std::ops::Range;

use js_sys::Reflect;
use wasm_bindgen::JsValue;

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
    Ternary(
        Box<Spanned<Expr>>,
        Box<Spanned<Expr>>,
        Box<Spanned<Expr>>,
    ),
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
    Question,
    Colon,
    Dot,
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
        Self { src: src.as_bytes(), pos: 0 }
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
            b'(' => { self.pos += 1; Tok::LParen }
            b')' => { self.pos += 1; Tok::RParen }
            b'?' => { self.pos += 1; Tok::Question }
            b':' => { self.pos += 1; Tok::Colon }
            b'.' => { self.pos += 1; Tok::Dot }
            b'!' => {
                self.pos += 1;
                if self.peek(0) == Some(b'=') { self.pos += 1; Tok::BangEq } else { Tok::Bang }
            }
            b'=' => {
                if self.peek(1) != Some(b'=') {
                    return Err(self.err(
                        start..start + 1,
                        "expected `==`",
                        Some("single `=` isn't an operator; use `==`"),
                    ));
                }
                self.pos += 2;
                Tok::EqEq
            }
            b'&' => {
                if self.peek(1) != Some(b'&') {
                    return Err(self.err(
                        start..start + 1,
                        "expected `&&`",
                        None,
                    ));
                }
                self.pos += 2;
                Tok::AndAnd
            }
            b'|' => {
                if self.peek(1) != Some(b'|') {
                    return Err(self.err(
                        start..start + 1,
                        "expected `||`",
                        None,
                    ));
                }
                self.pos += 2;
                Tok::OrOr
            }
            b'<' => {
                self.pos += 1;
                if self.peek(0) == Some(b'=') { self.pos += 1; Tok::Le } else { Tok::Lt }
            }
            b'>' => {
                self.pos += 1;
                if self.peek(0) == Some(b'=') { self.pos += 1; Tok::Ge } else { Tok::Gt }
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
        let n = s.parse::<f64>().map_err(|_| {
            ParseError {
                message: format!("invalid number literal {s:?}"),
                span: start..self.pos,
                hint: None,
            }
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
        let e = self.parse_expr_top()?;
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
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek().0 {
                Tok::Le => BinOp::Le,
                Tok::Lt => BinOp::Lt,
                Tok::Ge => BinOp::Ge,
                Tok::Gt => BinOp::Gt,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_unary()?;
            let span = lhs.span.start..rhs.span.end;
            lhs = Spanned {
                value: Expr::BinOp(op, Box::new(lhs), Box::new(rhs)),
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
                Ok(Spanned { value: Expr::Literal(Literal::String(s)), span })
            }
            Tok::NumberLit(n) => {
                self.pos += 1;
                Ok(Spanned { value: Expr::Literal(Literal::Number(n)), span })
            }
            Tok::True => {
                self.pos += 1;
                Ok(Spanned { value: Expr::Literal(Literal::Bool(true)), span })
            }
            Tok::False => {
                self.pos += 1;
                Ok(Spanned { value: Expr::Literal(Literal::Bool(false)), span })
            }
            Tok::Null => {
                self.pos += 1;
                Ok(Spanned { value: Expr::Literal(Literal::Null), span })
            }
            Tok::Ident(first) => {
                self.pos += 1;
                let mut segments = vec![first];
                let start = span.start;
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
                Ok(Spanned { value: Expr::Path(segments), span: start..end })
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

// ─── evaluator ────────────────────────────────────────────────────

/// Evaluate the AST against a scope proxy. The evaluator tracks deps
/// as a side effect of `Reflect::get` calls — short-circuited
/// branches don't run and therefore don't subscribe.
pub fn evaluate(expr: &Spanned<Expr>, scope: &JsValue) -> JsValue {
    match &expr.value {
        Expr::Literal(l) => lit_to_js(l),
        Expr::Path(segs) => resolve_segments(scope, segs),
        Expr::Not(inner) => JsValue::from_bool(evaluate(inner, scope).is_falsy()),
        Expr::BinOp(op, lhs, rhs) => match op {
            BinOp::And => {
                let l = evaluate(lhs, scope);
                if l.is_falsy() {
                    l
                } else {
                    evaluate(rhs, scope)
                }
            }
            BinOp::Or => {
                let l = evaluate(lhs, scope);
                if !l.is_falsy() {
                    l
                } else {
                    evaluate(rhs, scope)
                }
            }
            BinOp::Eq | BinOp::Ne => {
                let l = evaluate(lhs, scope);
                let r = evaluate(rhs, scope);
                let eq = js_strict_eq(&l, &r);
                let out = if matches!(op, BinOp::Eq) { eq } else { !eq };
                JsValue::from_bool(out)
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let l = evaluate(lhs, scope);
                let r = evaluate(rhs, scope);
                match (l.as_f64(), r.as_f64()) {
                    (Some(a), Some(b)) => JsValue::from_bool(match op {
                        BinOp::Lt => a < b,
                        BinOp::Le => a <= b,
                        BinOp::Gt => a > b,
                        BinOp::Ge => a >= b,
                        _ => unreachable!(),
                    }),
                    _ => JsValue::from_bool(false),
                }
            }
        },
        Expr::Ternary(cond, then_e, else_e) => {
            if !evaluate(cond, scope).is_falsy() {
                evaluate(then_e, scope)
            } else {
                evaluate(else_e, scope)
            }
        }
    }
}

fn lit_to_js(l: &Literal) -> JsValue {
    match l {
        Literal::Null => JsValue::NULL,
        Literal::Bool(b) => JsValue::from_bool(*b),
        Literal::Number(n) => JsValue::from_f64(*n),
        Literal::String(s) => JsValue::from_str(s),
    }
}

fn resolve_segments(root: &JsValue, segments: &[String]) -> JsValue {
    let mut cur = root.clone();
    for seg in segments {
        cur = Reflect::get(&cur, &JsValue::from_str(seg)).unwrap_or(JsValue::UNDEFINED);
    }
    cur
}

/// Strict equality — primitive types compare by value, everything
/// else compares by JS reference semantics. No type coercion.
fn js_strict_eq(a: &JsValue, b: &JsValue) -> bool {
    if let (Some(as_), Some(bs_)) = (a.as_string(), b.as_string()) {
        return as_ == bs_;
    }
    if let (Some(an), Some(bn)) = (a.as_f64(), b.as_f64()) {
        return an == bn;
    }
    if let (Some(ab), Some(bb)) = (a.as_bool(), b.as_bool()) {
        return ab == bb;
    }
    // Null / undefined quirks.
    if a.is_null() && b.is_null() {
        return true;
    }
    if a.is_undefined() && b.is_undefined() {
        return true;
    }
    // Fall back to referential equality via js_sys::Object::is.
    js_sys::Object::is(a, b)
}

/// Top-level truthiness evaluation — convenience for
/// `pp-show` / `pp-if`. Short form of `parse` + `evaluate` + `!is_falsy`.
pub fn evaluate_truthy(expr: &Spanned<Expr>, scope: &JsValue) -> bool {
    !evaluate(expr, scope).is_falsy()
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
        matches!(parse_ok("\"world\"").value, Expr::Literal(Literal::String(_)));
    }

    #[test]
    fn errors_carry_spans() {
        match parse("a ||") {
            Err(ParseError { span, .. }) => assert!(span.start >= 3),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn rejects_assignment() {
        parse_err("a = b");
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
}
