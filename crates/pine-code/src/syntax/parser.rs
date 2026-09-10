use super::{LanguageRegistry, SyntaxRequest, SyntaxResponse};
use crate::{
    TextRange,
    language::{HighlightError, HighlightLimits, Token, TokenKind, validate_tokens},
};
use std::{collections::BTreeMap, ops::ControlFlow};
use tree_sitter::{
    InputEdit, ParseOptions, Parser, Point, Query, QueryCursor, QueryCursorOptions,
    StreamingIterator, Tree,
};

/// Limits independent of application-configurable document sizes. Results may
/// fall back to plain text; parser failure never rejects a document transaction.
const MAX_BYTES: usize = 1024 * 1024;
const MAX_CAPTURES: usize = 64_000;
const MAX_PROGRESS: usize = 20_000;
const MAX_PAINT_BYTES: usize = 32 * 1024 * 1024;

pub struct SyntaxParser {
    languages: LanguageRegistry,
    queries: BTreeMap<String, Query>,
    parser: Parser,
    language: String,
    source: String,
    tree: Option<Tree>,
}
impl SyntaxParser {
    pub fn new(languages: LanguageRegistry) -> Self {
        Self {
            languages,
            queries: BTreeMap::new(),
            parser: Parser::new(),
            language: String::new(),
            source: String::new(),
            tree: None,
        }
    }
    pub fn process(&mut self, request: SyntaxRequest) -> SyntaxResponse {
        let result = self.highlight(&request.text, &request.version.language, request.limits);
        SyntaxResponse {
            version: request.version,
            result,
        }
    }
    pub fn highlight(
        &mut self,
        source: &str,
        language: &str,
        limits: HighlightLimits,
    ) -> Result<Vec<Vec<Token>>, HighlightError> {
        if source.len() > MAX_BYTES {
            return Err(HighlightError::DocumentBudget);
        }
        if source
            .split('\n')
            .any(|line| line.len() > limits.max_line_bytes)
        {
            return Err(HighlightError::LineBudget);
        }
        let definition = self
            .languages
            .get(language)
            .ok_or_else(|| HighlightError::UnknownLanguage(language.into()))?;
        if !self.queries.contains_key(language) {
            let mut query = Query::new(&definition.grammar.into(), &definition.highlights)
                .map_err(|e| HighlightError::InvalidLanguage(e.to_string()))?;
            // This API supplies syntax captures only. Rules requiring local
            // scope analysis are omitted; ordinary syntax patterns still apply.
            // Unsupported custom predicates must not silently become true.
            for i in 0..query.pattern_count() {
                if !query.general_predicates(i).is_empty() {
                    return Err(HighlightError::InvalidLanguage(
                        "unsupported query predicate".into(),
                    ));
                }
                if !query.property_predicates(i).is_empty() {
                    query.disable_pattern(i);
                }
            }
            self.queries.insert(language.into(), query);
        }
        if self.language != language {
            self.parser.reset();
            self.parser
                .set_language(&definition.grammar.into())
                .map_err(|e| HighlightError::InvalidLanguage(e.to_string()))?;
            self.tree = None;
            self.language = language.into();
        } else if let Some(tree) = &mut self.tree {
            tree.edit(&difference(&self.source, source));
        }
        let mut progress = 0;
        let mut budget = |_: &tree_sitter::ParseState| {
            progress += 1;
            if progress > MAX_PROGRESS {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let parsed = self.parser.parse_with_options(
            &mut |offset, _| &source.as_bytes()[offset..],
            self.tree.as_ref(),
            Some(ParseOptions::new().progress_callback(&mut budget)),
        );
        self.source = source.into();
        self.tree = parsed;
        let Some(tree) = &self.tree else {
            self.parser.reset();
            return Err(HighlightError::WorkBudget);
        };
        let query = &self.queries[language];
        let mut cursor = QueryCursor::new();
        cursor.set_match_limit(4096);
        let mut progress = 0;
        let mut cancelled = false;
        let mut budget = |_: &tree_sitter::QueryCursorState| {
            progress += 1;
            if progress > MAX_PROGRESS {
                cancelled = true;
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut captures = Vec::new();
        {
            let mut matches = cursor.captures_with_options(
                query,
                tree.root_node(),
                source.as_bytes(),
                QueryCursorOptions::new().progress_callback(&mut budget),
            );
            while let Some((m, index)) = matches.next() {
                let capture = m.captures()[*index];
                if let Some(kind) = capture_kind(query.capture_names()[capture.index as usize]) {
                    let range = capture.node.byte_range();
                    if range.start == range.end {
                        continue;
                    }
                    if !source.is_char_boundary(range.start) || !source.is_char_boundary(range.end)
                    {
                        return Err(HighlightError::InvalidRange);
                    }
                    captures.push((range, m.pattern_index, kind));
                    if captures.len() > MAX_CAPTURES {
                        return Err(HighlightError::WorkBudget);
                    }
                }
            }
        }
        if cancelled || cursor.did_exceed_match_limit() {
            return Err(HighlightError::WorkBudget);
        }
        // A nested capture overrides its parent. At identical node ranges,
        // later query patterns win, matching Tree-sitter 0.27 highlighting.
        captures.sort_by_key(|(range, pattern, _)| (std::cmp::Reverse(range.len()), *pattern));
        let mut colors = vec![None; source.len()];
        let mut work = 0;
        for (range, _, kind) in captures {
            work += range.len();
            if work > MAX_PAINT_BYTES {
                return Err(HighlightError::WorkBudget);
            }
            colors[range].fill(Some(kind));
        }
        let mut offset = 0;
        let mut spans = 0;
        let mut lines = Vec::new();
        for line in source.split('\n') {
            let mut tokens = Vec::new();
            let mut from = 0;
            while from < line.len() {
                let kind = colors[offset + from];
                let mut to = from + 1;
                while to < line.len() && colors[offset + to] == kind {
                    to += 1;
                }
                if let Some(kind) = kind {
                    tokens.push(Token {
                        range: TextRange::new(from, to),
                        kind,
                    });
                    spans += 1;
                    if spans > limits.max_spans {
                        return Err(HighlightError::TokenBudget);
                    }
                }
                from = to;
            }
            validate_tokens(line, &tokens)?;
            lines.push(tokens);
            offset += line.len() + 1;
        }
        Ok(lines)
    }
}

fn point(text: &str, offset: usize) -> Point {
    let prefix = &text[..offset];
    Point::new(
        prefix.bytes().filter(|&b| b == b'\n').count(),
        prefix.rfind('\n').map_or(offset, |i| offset - i - 1),
    )
}
fn difference(old: &str, new: &str) -> InputEdit {
    let mut start = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
        start -= 1;
    }
    let mut suffix = old.as_bytes()[start..]
        .iter()
        .rev()
        .zip(new.as_bytes()[start..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point(old, start),
        old_end_position: point(old, old_end),
        new_end_position: point(new, new_end),
    }
}
fn capture_kind(name: &str) -> Option<TokenKind> {
    use TokenKind::*;
    if name == "string.special.key" {
        return Some(Property);
    }
    Some(match name.split('.').next()? {
        "keyword" => Keyword,
        "string" | "character" => String,
        "number" | "float" => Number,
        "comment" => Comment,
        "constant" | "boolean" => Literal,
        "punctuation" => Punctuation,
        "label" | "lifetime" => Lifetime,
        "function" => Function,
        "type" | "constructor" => Type,
        "property" => Property,
        "variable" => Variable,
        "operator" => Operator,
        "attribute" => Attribute,
        "tag" => Tag,
        "escape" => Escape,
        _ => return None,
    })
}
