use super::{HighlightError, Language, LineStream, TokenKind};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BuiltinLanguage {
    #[default]
    Plain,
    Json,
    Rust,
}

impl BuiltinLanguage {
    pub fn resolve(name: &str) -> Result<Self, HighlightError> {
        match name {
            "" | "plain" | "text" => Ok(Self::Plain),
            "json" => Ok(Self::Json),
            "rust" => Ok(Self::Rust),
            _ => Err(HighlightError::UnknownLanguage(name.into())),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LexState {
    block_depth: u32,
    string: Option<StringMode>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum StringMode {
    Quoted,
    Raw(usize),
}

impl Language for BuiltinLanguage {
    type State = LexState;
    fn start_state(&self) -> LexState {
        LexState::default()
    }
    fn token(
        &self,
        stream: &mut LineStream<'_>,
        state: &mut LexState,
    ) -> Result<Option<TokenKind>, HighlightError> {
        if *self == Self::Plain {
            stream.advance(stream.rest().len())?;
            return Ok(None);
        }
        if state.block_depth > 0 {
            block_comment(stream, state);
            return Ok(Some(TokenKind::Comment));
        }
        if state.string.is_some() {
            string(stream, state);
            return Ok(Some(TokenKind::String));
        }
        let rest = stream.rest();
        if rest.starts_with(char::is_whitespace) {
            while stream.rest().starts_with(char::is_whitespace) {
                stream.next_char();
            }
            return Ok(None);
        }
        if *self == Self::Rust {
            if rest.starts_with("//") {
                stream.advance(rest.len())?;
                return Ok(Some(TokenKind::Comment));
            }
            if rest.starts_with("/*") {
                stream.advance(2)?;
                state.block_depth = 1;
                block_comment(stream, state);
                return Ok(Some(TokenKind::Comment));
            }
            // r"...", r#"..."#, br#"..."#, and raw C strings.
            let prefix = if rest.starts_with("br") || rest.starts_with("cr") {
                2
            } else if rest.starts_with('r') {
                1
            } else {
                0
            };
            if prefix != 0 {
                let hashes = rest[prefix..]
                    .bytes()
                    .take_while(|byte| *byte == b'#')
                    .count();
                if rest.as_bytes().get(prefix + hashes) == Some(&b'"') {
                    stream.advance(prefix + hashes + 1)?;
                    state.string = Some(StringMode::Raw(hashes));
                    string(stream, state);
                    return Ok(Some(TokenKind::String));
                }
            }
            if rest.starts_with("b\"") || rest.starts_with("c\"") {
                stream.advance(2)?;
                state.string = Some(StringMode::Quoted);
                string(stream, state);
                return Ok(Some(TokenKind::String));
            }
            let quote = if rest.starts_with("b'") {
                Some(1)
            } else if rest.starts_with('\'') {
                Some(0)
            } else {
                None
            };
            if let Some(prefix) = quote {
                if let Some(length) = char_literal(&rest[prefix..]) {
                    stream.advance(prefix + length)?;
                    return Ok(Some(TokenKind::String));
                }
                stream.advance(prefix + 1)?;
                while stream
                    .rest()
                    .starts_with(|ch: char| ch == '_' || ch.is_alphanumeric())
                {
                    stream.next_char();
                }
                return Ok(Some(TokenKind::Lifetime));
            }
        }
        if rest.starts_with('"') {
            stream.advance(1)?;
            state.string = Some(StringMode::Quoted);
            string(stream, state);
            return Ok(Some(TokenKind::String));
        }
        if rest.starts_with(|ch: char| ch.is_ascii_digit())
            || (*self == Self::Json && rest.starts_with('-'))
        {
            let mut previous = '\0';
            while let Some(ch) = stream.rest().chars().next() {
                let rest = stream.rest();
                if ch.is_ascii_alphanumeric()
                    || ch == '_'
                    || (ch == '.'
                        && !rest.starts_with("..")
                        && rest[1..].starts_with(|next: char| next.is_ascii_digit()))
                    || (matches!(ch, '+' | '-')
                        && (previous == '\0' || matches!(previous, 'e' | 'E')))
                {
                    stream.next_char();
                    previous = ch;
                } else {
                    break;
                }
            }
            return Ok(Some(TokenKind::Number));
        }
        if rest.starts_with(|ch: char| ch == '_' || ch.is_alphabetic()) {
            let start = stream.position();
            while stream
                .rest()
                .starts_with(|ch: char| ch == '_' || ch.is_alphanumeric())
            {
                stream.next_char();
            }
            let word = &rest[..stream.position() - start];
            if matches!(word, "true" | "false") || (*self == Self::Json && word == "null") {
                return Ok(Some(TokenKind::Literal));
            }
            if *self == Self::Rust
                && matches!(
                    word,
                    "as" | "async"
                        | "await"
                        | "break"
                        | "const"
                        | "continue"
                        | "crate"
                        | "dyn"
                        | "else"
                        | "enum"
                        | "extern"
                        | "fn"
                        | "for"
                        | "if"
                        | "impl"
                        | "in"
                        | "let"
                        | "loop"
                        | "match"
                        | "mod"
                        | "move"
                        | "mut"
                        | "pub"
                        | "ref"
                        | "return"
                        | "self"
                        | "Self"
                        | "static"
                        | "struct"
                        | "super"
                        | "trait"
                        | "type"
                        | "unsafe"
                        | "use"
                        | "where"
                        | "while"
                        | "yield"
                        | "union"
                        | "try"
                )
            {
                return Ok(Some(TokenKind::Keyword));
            }
            return Ok(None);
        }
        stream.next_char();
        Ok(Some(TokenKind::Punctuation))
    }
}

fn block_comment(stream: &mut LineStream<'_>, state: &mut LexState) {
    while !stream.rest().is_empty() {
        if stream.rest().starts_with("/*") {
            stream.position += 2;
            state.block_depth += 1;
        } else if stream.rest().starts_with("*/") {
            stream.position += 2;
            state.block_depth -= 1;
            if state.block_depth == 0 {
                break;
            }
        } else {
            stream.next_char();
        }
    }
}

fn string(stream: &mut LineStream<'_>, state: &mut LexState) {
    let mode = state.string.clone().unwrap();
    while let Some(ch) = stream.next_char() {
        match mode {
            StringMode::Quoted if ch == '\\' => {
                stream.next_char();
            }
            StringMode::Quoted if ch == '"' => {
                state.string = None;
                break;
            }
            StringMode::Raw(hashes)
                if ch == '"'
                    && stream
                        .rest()
                        .bytes()
                        .take(hashes)
                        .filter(|b| *b == b'#')
                        .count()
                        == hashes =>
            {
                stream.position += hashes;
                state.string = None;
                break;
            }
            _ => (),
        }
    }
}

fn char_literal(text: &str) -> Option<usize> {
    let mut chars = text[1..].char_indices();
    let (_, first) = chars.next()?;
    if first == '\\' {
        let (_, escape) = chars.next()?;
        if escape == 'u' {
            if chars.next()?.1 != '{' {
                return None;
            }
            for (_, ch) in chars.by_ref() {
                if ch == '}' {
                    break;
                }
                if !ch.is_ascii_hexdigit() && ch != '_' {
                    return None;
                }
            }
        } else if escape == 'x' {
            chars.next()?;
            chars.next()?;
        }
    } else if first == '\'' {
        return None;
    }
    let (offset, closing) = chars.next()?;
    (closing == '\'').then_some(offset + 2)
}
