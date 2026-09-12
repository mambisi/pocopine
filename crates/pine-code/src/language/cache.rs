use super::{
    BuiltinLanguage, HighlightError, HighlightLimits, Language, LexState, PresentationStatus,
    Token, tokenize_line,
};
use crate::TextDocument;

#[derive(Clone)]
struct CachedLine {
    text: String,
    incoming: LexState,
    outgoing: LexState,
    tokens: Vec<Token>,
    valid: bool,
}

pub struct HighlightCache {
    lines: Vec<CachedLine>,
    language: BuiltinLanguage,
    limits: HighlightLimits,
    next: usize,
    spans: usize,
    pub status: PresentationStatus,
}

pub struct HighlightStep {
    pub line: usize,
    pub tokens: Vec<Token>,
}

impl HighlightCache {
    pub fn invalidate(&mut self) {
        self.lines.clear();
        self.spans = 0;
        self.next = 0;
        self.status = PresentationStatus::Plain;
    }
    pub fn new(limits: HighlightLimits) -> Self {
        Self {
            lines: Vec::new(),
            language: BuiltinLanguage::Plain,
            limits,
            next: 0,
            spans: 0,
            status: PresentationStatus::Plain,
        }
    }

    pub fn update(&mut self, document: &TextDocument, name: &str) {
        let language = match BuiltinLanguage::resolve(name) {
            Ok(language) => language,
            Err(error) => {
                self.fallback(error);
                return;
            }
        };
        // Retrying a document-wide span overflow on every keystroke creates
        // repeated background scans. Keep plain mode until load or a language
        // change explicitly starts a new highlighting pass.
        if self.language == language
            && self.status == PresentationStatus::PlainFallback(HighlightError::TokenBudget)
        {
            return;
        }
        if language == BuiltinLanguage::Plain {
            self.lines.clear();
            self.next = 0;
            self.spans = 0;
            self.language = language;
            self.status = PresentationStatus::Plain;
            return;
        }
        if (0..document.line_count())
            .any(|line| document.line(line).unwrap().len() > self.limits.max_line_bytes)
        {
            self.fallback(HighlightError::LineBudget);
            return;
        }
        if self.language != language {
            self.lines.clear();
            self.spans = 0;
        }
        self.language = language;
        let mut prefix = 0;
        while prefix < self.lines.len().min(document.line_count())
            && self.lines[prefix].text == document.line(prefix).unwrap()
        {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < self.lines.len() - prefix
            && suffix < document.line_count() - prefix
            && self.lines[self.lines.len() - 1 - suffix].text
                == document.line(document.line_count() - 1 - suffix).unwrap()
        {
            suffix += 1;
        }
        let old_end = self.lines.len() - suffix;
        let new_end = document.line_count() - suffix;
        let initial = language.start_state();
        self.lines.splice(
            prefix..old_end,
            (prefix..new_end).map(|line| CachedLine {
                text: document.line(line).unwrap().into(),
                incoming: initial.clone(),
                outgoing: initial.clone(),
                tokens: Vec::new(),
                valid: false,
            }),
        );
        self.spans = self.lines.iter().map(|line| line.tokens.len()).sum();
        // If a prior frame has not reached this change, resume from its first
        // invalid line; suffix reuse requires matching incoming lexical state.
        self.next = self
            .lines
            .iter()
            .position(|line| !line.valid)
            .unwrap_or(prefix)
            .min(prefix);
        self.status = if self.next == self.lines.len() {
            PresentationStatus::Highlighted
        } else {
            PresentationStatus::Pending
        };
    }

    pub fn next_line(&mut self) -> Result<Option<HighlightStep>, HighlightError> {
        if self.status != PresentationStatus::Pending {
            return Ok(None);
        }
        let index = self.next;
        if index >= self.lines.len() {
            self.status = PresentationStatus::Highlighted;
            return Ok(None);
        }
        let incoming = if index == 0 {
            self.language.start_state()
        } else {
            self.lines[index - 1].outgoing.clone()
        };
        let line = &mut self.lines[index];
        if line.valid && line.incoming == incoming {
            // Every later line is unchanged and already converged, unless it
            // belongs to unfinished work from a preceding document revision.
            self.next = self.lines[index..]
                .iter()
                .position(|line| !line.valid)
                .map_or(self.lines.len(), |offset| index + offset);
            if self.next == self.lines.len() {
                self.status = PresentationStatus::Highlighted;
                return Ok(None);
            }
            return self.next_line();
        }
        let result = match tokenize_line(&self.language, &line.text, &incoming, self.limits) {
            Ok(result) => result,
            Err(error) => {
                self.fallback(error.clone());
                return Err(error);
            }
        };
        self.spans = self.spans - line.tokens.len() + result.tokens.len();
        if self.spans > self.limits.max_spans {
            self.fallback(HighlightError::TokenBudget);
            return Err(HighlightError::TokenBudget);
        }
        line.incoming = incoming;
        line.outgoing = result.state;
        line.tokens = result.tokens.clone();
        line.valid = true;
        self.next += 1;
        if self.next == self.lines.len() {
            self.status = PresentationStatus::Highlighted;
        }
        Ok(Some(HighlightStep {
            line: index,
            tokens: result.tokens,
        }))
    }

    fn fallback(&mut self, error: HighlightError) {
        self.lines.clear();
        self.next = 0;
        self.spans = 0;
        self.status = PresentationStatus::PlainFallback(error);
    }
}
