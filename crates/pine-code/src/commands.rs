use crate::{
    Change, ChangeSet, CodeError, CodeResult, EditOrigin, EditorState, Selection, TextOffset,
    Transaction,
};

/// Enter copies only indentation before the selection's insertion point.
pub fn insert_newline(state: &EditorState) -> CodeResult<Transaction> {
    let range = state.selection().range();
    let doc = state.document();
    let line = doc.line_at(range.from)?;
    let before = &doc.text()[doc.line_start(line).unwrap()..range.from.0];
    let indentation = before
        .chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .collect::<String>();
    replace_selection(state, &format!("\n{indentation}"), EditOrigin::Command)
}

pub fn replace_selection(
    state: &EditorState,
    text: &str,
    origin: EditOrigin,
) -> CodeResult<Transaction> {
    let range = state.selection().range();
    let text = crate::normalize_lf(text);
    let mut transaction = state.transaction(
        ChangeSet::new(
            state.document(),
            vec![Change::replace(range.from.0, range.to.0, &text)],
        )?,
        origin,
    );
    transaction.selection = Some(Selection::caret(range.from.0 + text.len()));
    Ok(transaction)
}

/// Indent complete selected lines; a terminal line-start is excluded. For a
/// collapsed caret, indentation inserts to the next stop (or a literal tab).
pub fn indent_lines(
    state: &EditorState,
    unit: &str,
    outdent: bool,
    tab_size: usize,
) -> CodeResult<Transaction> {
    if !(1..=32).contains(&tab_size)
        || unit.is_empty()
        || unit.len() > 32
        || (unit != "\t" && !unit.bytes().all(|b| b == b' '))
    {
        return Err(CodeError::InvalidConfiguration);
    }
    let doc = state.document();
    let selection = state.selection();
    let range = selection.range();
    let first = doc.line_at(range.from)?;
    if selection.is_empty() && !outdent {
        let before = &doc.text()[doc.line_start(first).unwrap()..range.from.0];
        let column = before.chars().fold(0, |column, ch| {
            if ch == '\t' {
                column + tab_size - column % tab_size
            } else {
                column + 1
            }
        });
        let insert = if unit == "\t" {
            unit.into()
        } else {
            " ".repeat(unit.len() - column % unit.len())
        };
        return replace_selection(state, &insert, EditOrigin::Command);
    }
    let mut last = doc.line_at(range.to)?;
    if last > first && doc.line_start(last) == Some(range.to.0) {
        last -= 1;
    }
    let mut changes = Vec::new();
    for index in first..=last {
        let from = doc.line_start(index).unwrap();
        let line = doc.line(index).unwrap();
        if outdent {
            let count = if line.starts_with('\t') {
                1
            } else {
                line.bytes()
                    .take_while(|b| *b == b' ')
                    .count()
                    .min(if unit == "\t" { tab_size } else { unit.len() })
            };
            if count != 0 {
                changes.push(Change::replace(from, from + count, ""));
            }
        } else {
            changes.push(Change::replace(from, from, unit));
        }
    }
    let changes = ChangeSet::new(doc, changes)?;
    let mapped = changes.map_selection(doc, selection)?;
    let mut transaction = state.transaction(changes, EditOrigin::Command);
    transaction.selection = Some(mapped);
    Ok(transaction)
}

/// Select all is revision checked by dispatch, including an empty document.
pub fn select_all(state: &EditorState) -> Transaction {
    let mut transaction = state.transaction(ChangeSet::default(), EditOrigin::Command);
    transaction.selection = Some(Selection {
        anchor: TextOffset(0),
        head: TextOffset(state.document().len()),
    });
    transaction
}
