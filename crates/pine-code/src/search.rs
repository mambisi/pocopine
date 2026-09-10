use crate::{
    ChangeSet, CodeError, CodeResult, DocumentLimits, DocumentRevision, EditOrigin, EditorState,
    TextDocument, TextRange, Transaction,
};
use serde::Serialize;

pub const MAX_CACHED_MATCHES: usize = 10_000;

#[derive(Clone, Debug, Serialize)]
pub struct SearchResults {
    pub revision: DocumentRevision,
    pub total: usize,
    /// Bounded display cache. Navigation/replacement scan the entire document.
    pub matches: Vec<TextRange>,
}

pub fn search(state: &EditorState, query: &str) -> SearchResults {
    let mut result = SearchResults {
        revision: state.revision(),
        total: 0,
        matches: Vec::new(),
    };
    if query.is_empty() {
        return result;
    }
    for (offset, matched) in state.document().text().match_indices(query) {
        result.total += 1;
        if result.matches.len() < MAX_CACHED_MATCHES {
            result
                .matches
                .push(TextRange::new(offset, offset + matched.len()));
        }
    }
    result
}

/// Literal, non-overlapping search with one wrap, independent of the cache cap.
pub fn next_match(state: &EditorState, query: &str, backwards: bool) -> Option<TextRange> {
    if query.is_empty() {
        return None;
    }
    let range = state.selection().range();
    let mut first = None;
    let mut last = None;
    let mut previous = None;
    for (offset, matched) in state.document().text().match_indices(query) {
        let found = TextRange::new(offset, offset + matched.len());
        first.get_or_insert(found);
        last = Some(found);
        if backwards {
            if found.to <= range.from {
                previous = Some(found);
            }
        } else if found.from >= range.to {
            return Some(found);
        }
    }
    if backwards { previous.or(last) } else { first }
}

pub fn replace_current(
    state: &EditorState,
    query: &str,
    replacement: &str,
) -> CodeResult<Option<Transaction>> {
    if query.is_empty() || state.document().slice(state.selection().range())? != query {
        return Ok(None);
    }
    crate::commands::replace_selection(state, replacement, EditOrigin::Command).map(Some)
}

/// Build a single undoable replacement with bounded memory, even when more
/// matches exist than the display cache can hold. Enforce growth as we append.
pub fn replace_all(
    state: &EditorState,
    query: &str,
    replacement: &str,
    limits: DocumentLimits,
) -> CodeResult<Transaction> {
    if query.is_empty() {
        return Ok(state.transaction(ChangeSet::default(), EditOrigin::Command));
    }
    let source = state.document().text();
    let replacement = crate::normalize_lf(replacement);
    let mut text = String::new();
    let mut end = 0;
    for (offset, matched) in source.match_indices(query) {
        let size = text
            .len()
            .checked_add(offset - end)
            .and_then(|len| len.checked_add(replacement.len()))
            .ok_or(CodeError::SizeLimit)?;
        if size > limits.max_bytes {
            return Err(CodeError::SizeLimit);
        }
        text.push_str(&source[end..offset]);
        text.push_str(&replacement);
        end = offset + matched.len();
    }
    if text
        .len()
        .checked_add(source.len() - end)
        .ok_or(CodeError::SizeLimit)?
        > limits.max_bytes
    {
        return Err(CodeError::SizeLimit);
    }
    text.push_str(&source[end..]);
    TextDocument::new(&text, limits)?;
    Ok(state.transaction(
        ChangeSet::between(state.document(), &text)?,
        EditOrigin::Command,
    ))
}
