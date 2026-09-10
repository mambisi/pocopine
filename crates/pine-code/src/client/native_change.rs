use crate::{
    Change, ChangeSet, CodeResult, DocumentLimits, DocumentRevision, Selection, TextDocument,
    TextRange,
};

pub(super) struct InputIntent {
    pub revision: DocumentRevision,
    pub selection: Selection,
    pub input_type: String,
    pub data: Option<String>,
}

/// A minimal diff alone slides an insertion/deletion through repeated text.
/// Prefer the pre-input caret/selection when it reconstructs the exact candidate.
pub(super) fn changes_from_region(
    document: &TextDocument,
    range: TextRange,
    text: &str,
    limits: DocumentLimits,
    intent: Option<&InputIntent>,
) -> CodeResult<ChangeSet> {
    let old = document.slice(range)?;
    if let Some(intent) = intent {
        let selection = intent.selection.range();
        let hinted = if intent.input_type.starts_with("insert") {
            intent
                .data
                .as_ref()
                .map(|data| Change::replace(selection.from.0, selection.to.0, data))
        } else if intent.input_type.starts_with("delete") {
            if !selection.is_empty() {
                Some(Change::replace(selection.from.0, selection.to.0, ""))
            } else if let Some(deleted) = old
                .len()
                .checked_sub(text.len())
                .filter(|length| *length > 0)
            {
                let head = selection.from.0;
                if intent.input_type.ends_with("Backward") {
                    head.checked_sub(deleted)
                        .map(|from| Change::replace(from, head, ""))
                } else if intent.input_type.ends_with("Forward") {
                    head.checked_add(deleted)
                        .map(|to| Change::replace(head, to, ""))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        if let Some(mut hinted) = hinted.filter(|hint| {
            hint.range.from >= range.from
                && hint.range.to <= range.to
                && document.slice(hint.range).is_ok()
        }) {
            hinted.insert = crate::normalize_lf(&hinted.insert);
            let from = hinted.range.from.0 - range.from.0;
            let to = hinted.range.to.0 - range.from.0;
            if text.len() == from + hinted.insert.len() + old.len() - to
                && text.starts_with(&old[..from])
                && text[from..].starts_with(&hinted.insert)
                && text[from + hinted.insert.len()..] == old[to..]
            {
                return ChangeSet::new(document, vec![hinted]);
            }
        }
    }
    let region = TextDocument::new(old, limits)?;
    let local = ChangeSet::between(&region, text)?;
    ChangeSet::new(
        document,
        local
            .changes()
            .iter()
            .map(|change| {
                Change::replace(
                    range.from.0 + change.range.from.0,
                    range.from.0 + change.range.to.0,
                    &change.insert,
                )
            })
            .collect(),
    )
}
