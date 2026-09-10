use serde::{Deserialize, Serialize};

use crate::{CodeError, CodeResult};

/// Byte offsets are validated against their document at every API boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TextOffset(pub usize);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DocumentRevision(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRange {
    pub from: TextOffset,
    pub to: TextOffset,
}

impl TextRange {
    pub const fn new(from: usize, to: usize) -> Self {
        Self {
            from: TextOffset(from),
            to: TextOffset(to),
        }
    }

    pub fn is_empty(self) -> bool {
        self.from == self.to
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub anchor: TextOffset,
    pub head: TextOffset,
}

impl Selection {
    pub const fn caret(offset: usize) -> Self {
        Self {
            anchor: TextOffset(offset),
            head: TextOffset(offset),
        }
    }

    pub const fn between(anchor: usize, head: usize) -> Self {
        Self {
            anchor: TextOffset(anchor),
            head: TextOffset(head),
        }
    }

    pub fn range(self) -> TextRange {
        TextRange {
            from: self.anchor.min(self.head),
            to: self.anchor.max(self.head),
        }
    }

    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentLimits {
    pub max_bytes: usize,
    pub max_lines: usize,
}

impl Default for DocumentLimits {
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024,
            max_lines: 10_000,
        }
    }
}

impl DocumentLimits {
    pub fn validate(self) -> CodeResult<()> {
        if self.max_bytes == 0 || self.max_lines == 0 {
            Err(CodeError::InvalidConfiguration)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextDocument {
    text: String,
    lines: Vec<usize>,
}

impl TextDocument {
    pub fn new(text: &str, limits: DocumentLimits) -> CodeResult<Self> {
        limits.validate()?;
        let text = normalize_lf(text);
        if text.len() > limits.max_bytes {
            return Err(CodeError::SizeLimit);
        }
        let mut lines = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                lines.push(i + 1);
            }
            if lines.len() > limits.max_lines {
                return Err(CodeError::SizeLimit);
            }
        }
        Ok(Self { text, lines })
    }

    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn len(&self) -> usize {
        self.text.len()
    }
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    pub fn line_start(&self, index: usize) -> Option<usize> {
        self.lines.get(index).copied()
    }
    pub fn line(&self, index: usize) -> Option<&str> {
        let start = self.line_start(index)?;
        let end = self
            .line_start(index + 1)
            .map_or(self.len(), |next| next - 1);
        Some(&self.text[start..end])
    }

    pub fn line_at(&self, pos: TextOffset) -> CodeResult<usize> {
        self.check_offset(pos)?;
        Ok(self.lines.partition_point(|start| *start <= pos.0) - 1)
    }

    pub fn check_offset(&self, pos: TextOffset) -> CodeResult<()> {
        if self.text.is_char_boundary(pos.0) {
            Ok(())
        } else {
            Err(CodeError::InvalidPosition)
        }
    }

    pub fn check_selection(&self, selection: Selection) -> CodeResult<()> {
        self.check_offset(selection.anchor)?;
        self.check_offset(selection.head)
    }

    pub fn slice(&self, range: TextRange) -> CodeResult<&str> {
        if range.from > range.to {
            return Err(CodeError::InvalidRange);
        }
        self.text
            .get(range.from.0..range.to.0)
            .ok_or(CodeError::InvalidPosition)
    }

    pub fn export_text(&self, ending: LineEnding) -> String {
        match ending {
            LineEnding::Lf => self.text.clone(),
            LineEnding::CrLf => self.text.replace('\n', "\r\n"),
        }
    }
}

pub fn normalize_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn byte_to_utf16(text: &str, byte: usize) -> CodeResult<u32> {
    let prefix = text.get(..byte).ok_or(CodeError::InvalidPosition)?;
    u32::try_from(prefix.encode_utf16().count()).map_err(|_| CodeError::InvalidPosition)
}

pub fn utf16_to_byte(text: &str, offset: u32) -> CodeResult<usize> {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units == offset {
            return Ok(byte);
        }
        units += ch.len_utf16() as u32;
        if units > offset {
            return Err(CodeError::InvalidPosition);
        }
    }
    if units == offset {
        Ok(text.len())
    } else {
        Err(CodeError::InvalidPosition)
    }
}
