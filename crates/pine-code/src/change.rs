use serde::{Deserialize, Serialize};

use crate::{
    CodeError, CodeResult, DocumentLimits, Selection, TextDocument, TextOffset, TextRange,
    normalize_lf,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bias {
    Before,
    After,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub range: TextRange,
    pub insert: String,
}

impl Change {
    pub fn replace(from: usize, to: usize, insert: impl Into<String>) -> Self {
        Self {
            range: TextRange::new(from, to),
            insert: insert.into(),
        }
    }
}

/// Changes use coordinates in one pre-edit document. Private storage ensures
/// mapping and inversion only operate on validated, normalized changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ChangeSet {
    changes: Vec<Change>,
}

impl ChangeSet {
    pub fn new(doc: &TextDocument, mut changes: Vec<Change>) -> CodeResult<Self> {
        for change in &mut changes {
            doc.slice(change.range)?;
            change.insert = normalize_lf(&change.insert);
        }
        changes.retain(|change| !change.range.is_empty() || !change.insert.is_empty());
        changes.sort_by_key(|change| (change.range.from, change.range.to));
        for pair in changes.windows(2) {
            if pair[0].range.to > pair[1].range.from
                || (pair[0].range.to == pair[1].range.from
                    && (pair[0].range.is_empty() || pair[1].range.is_empty()))
            {
                return Err(CodeError::OverlappingChanges);
            }
        }
        Ok(Self { changes })
    }

    pub fn changes(&self) -> &[Change] {
        &self.changes
    }
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn apply(&self, doc: &TextDocument, limits: DocumentLimits) -> CodeResult<TextDocument> {
        let mut size = doc.len();
        for change in &self.changes {
            doc.slice(change.range)?;
            size = size
                .checked_sub(change.range.to.0 - change.range.from.0)
                .and_then(|n| n.checked_add(change.insert.len()))
                .ok_or(CodeError::SizeLimit)?;
        }
        if size > limits.max_bytes {
            return Err(CodeError::SizeLimit);
        }
        let mut text = String::with_capacity(size);
        let mut offset = 0;
        for change in &self.changes {
            text.push_str(&doc.text()[offset..change.range.from.0]);
            text.push_str(&change.insert);
            offset = change.range.to.0;
        }
        text.push_str(&doc.text()[offset..]);
        TextDocument::new(&text, limits)
    }

    pub fn inverse(&self, doc: &TextDocument, limits: DocumentLimits) -> CodeResult<Self> {
        let result = self.apply(doc, limits)?;
        let mut inverse: Vec<Change> = Vec::new();
        let mut old_end = 0;
        let mut new_end = 0;
        for change in &self.changes {
            let from = new_end + change.range.from.0 - old_end;
            let to = from + change.insert.len();
            let insert = doc.slice(change.range)?.to_owned();
            if let Some(last) = inverse.last_mut().filter(|last| last.range.to.0 == from) {
                last.range.to = TextOffset(to);
                last.insert.push_str(&insert);
            } else {
                inverse.push(Change::replace(from, to, insert));
            }
            old_end = change.range.to.0;
            new_end = to;
        }
        Self::new(&result, inverse)
    }

    pub fn map_offset(
        &self,
        doc: &TextDocument,
        offset: TextOffset,
        bias: Bias,
    ) -> CodeResult<TextOffset> {
        doc.check_offset(offset)?;
        for change in &self.changes {
            doc.slice(change.range)?;
        }
        Ok(self.map_valid_offset(offset, bias))
    }

    fn map_valid_offset(&self, offset: TextOffset, bias: Bias) -> TextOffset {
        let mut old_end = 0;
        let mut new_end = 0;
        for change in &self.changes {
            if offset < change.range.from {
                break;
            }
            let start = new_end + change.range.from.0 - old_end;
            if offset < change.range.to || (change.range.is_empty() && offset == change.range.from)
            {
                return TextOffset(
                    start
                        + if bias == Bias::After {
                            change.insert.len()
                        } else {
                            0
                        },
                );
            }
            old_end = change.range.to.0;
            new_end = start + change.insert.len();
        }
        TextOffset(new_end + offset.0 - old_end)
    }

    pub fn map_selection(&self, doc: &TextDocument, selection: Selection) -> CodeResult<Selection> {
        doc.check_selection(selection)?;
        for change in &self.changes {
            doc.slice(change.range)?;
        }
        if selection.is_empty() {
            return Ok(Selection::caret(
                self.map_valid_offset(selection.head, Bias::After).0,
            ));
        }
        let range = selection.range();
        let lower = self.map_valid_offset(range.from, Bias::After);
        let upper = self.map_valid_offset(range.to, Bias::Before);
        if lower > upper {
            return Ok(Selection::caret(lower.0));
        }
        if selection.anchor <= selection.head {
            Ok(Selection::between(lower.0, upper.0))
        } else {
            Ok(Selection::between(upper.0, lower.0))
        }
    }

    /// Minimal scalar-aligned contiguous difference, for DOM input and history.
    pub fn between(doc: &TextDocument, text: &str) -> CodeResult<Self> {
        let text = normalize_lf(text);
        let prefix = doc
            .text()
            .chars()
            .zip(text.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum::<usize>();
        let suffix = doc.text()[prefix..]
            .chars()
            .rev()
            .zip(text[prefix..].chars().rev())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum::<usize>();
        Self::new(
            doc,
            vec![Change::replace(
                prefix,
                doc.len() - suffix,
                &text[prefix..text.len() - suffix],
            )],
        )
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.changes.capacity() * std::mem::size_of::<Change>()
            + self
                .changes
                .iter()
                .map(|c| c.insert.capacity())
                .sum::<usize>()
    }
}
