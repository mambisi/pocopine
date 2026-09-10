//! Bounded line tokenization. Lexical snapshots are owned values; editing does
//! not depend on highlighting succeeding. See NOTICE.codemirror.md.

mod builtin;
mod cache;
pub use builtin::{BuiltinLanguage, LexState};
pub use cache::{HighlightCache, HighlightStep};

use crate::TextRange;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    Keyword,
    String,
    Number,
    Comment,
    Literal,
    Punctuation,
    Lifetime,
}
impl TokenKind {
    pub fn class(self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::String => "string",
            Self::Number => "number",
            Self::Comment => "comment",
            Self::Literal => "literal",
            Self::Punctuation => "punctuation",
            Self::Lifetime => "lifetime",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub range: TextRange,
    pub kind: TokenKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HighlightError {
    LineBudget,
    TokenBudget,
    InvalidRange,
    NoProgress,
    UnknownLanguage(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresentationStatus {
    Plain,
    Pending,
    Highlighted,
    PlainFallback(HighlightError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightLimits {
    pub max_spans: usize,
    pub max_line_bytes: usize,
    pub slice_ms: u32,
}
impl Default for HighlightLimits {
    fn default() -> Self {
        Self {
            max_spans: 8_000,
            max_line_bytes: 8 * 1024,
            slice_ms: 4,
        }
    }
}

pub struct LineStream<'a> {
    text: &'a str,
    position: usize,
}
impl<'a> LineStream<'a> {
    pub fn rest(&self) -> &'a str {
        &self.text[self.position..]
    }
    pub fn position(&self) -> usize {
        self.position
    }
    pub fn advance(&mut self, bytes: usize) -> Result<(), HighlightError> {
        let next = self
            .position
            .checked_add(bytes)
            .ok_or(HighlightError::InvalidRange)?;
        if !self.text.is_char_boundary(next) {
            return Err(HighlightError::InvalidRange);
        }
        self.position = next;
        Ok(())
    }
    pub fn next_char(&mut self) -> Option<char> {
        let ch = self.rest().chars().next()?;
        self.position += ch.len_utf8();
        Some(ch)
    }
}

/// `token` must return promptly. The driver rejects ten consecutive state-only
/// steps; built-in loops always advance a scalar. Blank lines update state too.
pub trait Language {
    type State: Clone + PartialEq + Eq;
    fn start_state(&self) -> Self::State;
    fn token(
        &self,
        stream: &mut LineStream<'_>,
        state: &mut Self::State,
    ) -> Result<Option<TokenKind>, HighlightError>;
    fn blank_line(&self, _state: &mut Self::State) {}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenizedLine<S> {
    pub tokens: Vec<Token>,
    pub state: S,
}

pub fn validate_tokens(line: &str, tokens: &[Token]) -> Result<(), HighlightError> {
    let mut end = 0;
    for token in tokens {
        if token.range.from.0 < end
            || token.range.from >= token.range.to
            || !line.is_char_boundary(token.range.from.0)
            || !line.is_char_boundary(token.range.to.0)
        {
            return Err(HighlightError::InvalidRange);
        }
        end = token.range.to.0;
    }
    Ok(())
}

pub fn tokenize_line<L: Language>(
    language: &L,
    line: &str,
    incoming: &L::State,
    limits: HighlightLimits,
) -> Result<TokenizedLine<L::State>, HighlightError> {
    if line.len() > limits.max_line_bytes {
        return Err(HighlightError::LineBudget);
    }
    let mut state = incoming.clone();
    let mut tokens: Vec<Token> = Vec::new();
    if line.is_empty() {
        language.blank_line(&mut state);
    }
    let mut stream = LineStream {
        text: line,
        position: 0,
    };
    let mut stalled = 0;
    while !stream.rest().is_empty() {
        let start = stream.position;
        let kind = language.token(&mut stream, &mut state)?;
        if stream.position == start {
            stalled += 1;
            if stalled >= 10 {
                return Err(HighlightError::NoProgress);
            }
            continue;
        }
        stalled = 0;
        if let Some(kind) = kind {
            if let Some(last) = tokens
                .last_mut()
                .filter(|token| token.kind == kind && token.range.to.0 == start)
            {
                last.range.to.0 = stream.position;
            } else {
                tokens.push(Token {
                    range: TextRange::new(start, stream.position),
                    kind,
                });
            }
            if tokens.len() > limits.max_spans {
                return Err(HighlightError::TokenBudget);
            }
        }
    }
    validate_tokens(line, &tokens)?;
    Ok(TokenizedLine { tokens, state })
}
