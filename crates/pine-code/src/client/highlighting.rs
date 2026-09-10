use super::{
    runtime::Runtime,
    syntax_worker::{self, WorkerClient, WorkerEvent},
};
use crate::{
    DocumentRevision, TextDocument,
    language::{
        HighlightError, HighlightLimits, HighlightStep, PresentationStatus, Token, validate_tokens,
    },
    syntax::{SyntaxRequest, SyntaxVersion},
};
use std::rc::Weak;

pub(super) struct HighlightCache {
    pub status: PresentationStatus,
    desired: Option<SyntaxVersion>,
    generation: u64,
    in_flight: Option<SyntaxVersion>,
    worker: Option<WorkerClient>,
    settings: Option<syntax_worker::Settings>,
    lines: Option<Vec<Vec<Token>>>,
    next: usize,
}
impl HighlightCache {
    pub fn new() -> Self {
        Self {
            status: PresentationStatus::Plain,
            desired: None,
            generation: 0,
            in_flight: None,
            worker: None,
            settings: syntax_worker::settings(),
            lines: None,
            next: 0,
        }
    }
    pub fn update(
        &mut self,
        document: &TextDocument,
        revision: DocumentRevision,
        language: &str,
        reset: bool,
    ) {
        let same_language = self
            .desired
            .as_ref()
            .is_some_and(|v| v.language == language);
        self.generation += 1;
        self.desired = Some(SyntaxVersion {
            generation: self.generation,
            revision,
            language: language.into(),
        });
        self.lines = None;
        self.next = 0;
        if !reset
            && same_language
            && matches!(
                self.status,
                PresentationStatus::PlainFallback(
                    HighlightError::TokenBudget
                        | HighlightError::WorkerFailed(_)
                        | HighlightError::InvalidLanguage(_)
                )
            )
        {
            return;
        }
        if matches!(language, "" | "plain" | "text") {
            self.stop();
            self.status = PresentationStatus::Plain;
            return;
        }
        let Some(settings) = &self.settings else {
            self.fallback(HighlightError::WorkerUnavailable);
            return;
        };
        if !settings.indents.contains_key(language) {
            self.fallback(HighlightError::UnknownLanguage(language.into()));
            return;
        }
        if document.len() > 1024 * 1024 {
            self.fallback(HighlightError::DocumentBudget);
            return;
        }
        if (0..document.line_count())
            .any(|i| document.line(i).unwrap().len() > HighlightLimits::default().max_line_bytes)
        {
            self.fallback(HighlightError::LineBudget);
            return;
        }
        self.status = PresentationStatus::Pending;
    }
    pub fn submit(&mut self, document: &TextDocument, weak: &Weak<Runtime>) {
        if self.status != PresentationStatus::Pending
            || self.lines.is_some()
            || self.in_flight.is_some()
        {
            return;
        }
        if self.worker.is_none() {
            match WorkerClient::new(&self.settings.as_ref().unwrap().config, weak) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => {
                    self.fallback(HighlightError::WorkerFailed(format!("{error:?}")));
                    return;
                }
            }
        }
        let worker = self.worker.as_mut().unwrap();
        if !worker.ready {
            return;
        }
        let request = SyntaxRequest {
            version: self.desired.clone().unwrap(),
            text: document.text().into(),
            limits: HighlightLimits::default(),
        };
        if let Err(error) = worker.send(&request, weak) {
            self.fallback(HighlightError::WorkerFailed(format!("{error:?}")));
        } else {
            self.in_flight = Some(request.version);
        }
    }
    pub fn receive(&mut self, event: WorkerEvent, document: &TextDocument) {
        match event {
            WorkerEvent::Ready => {
                if let Some(worker) = &mut self.worker {
                    worker.mark_ready();
                }
            }
            WorkerEvent::Failed(error) => self.fallback(HighlightError::WorkerFailed(error)),
            WorkerEvent::Result(response) => {
                if self.in_flight.as_ref() != Some(&response.version) {
                    return;
                }
                self.in_flight = None;
                if let Some(worker) = &mut self.worker {
                    worker.clear_timeout();
                }
                if self.desired.as_ref() != Some(&response.version) {
                    return;
                }
                match response.result {
                    Ok(lines) => {
                        if lines.len() != document.line_count()
                            || lines.iter().enumerate().any(|(i, tokens)| {
                                validate_tokens(document.line(i).unwrap(), tokens).is_err()
                            })
                        {
                            self.fallback(HighlightError::InvalidRange);
                            return;
                        }
                        if lines.iter().map(Vec::len).sum::<usize>()
                            > HighlightLimits::default().max_spans
                        {
                            self.fallback(HighlightError::TokenBudget);
                            return;
                        }
                        self.lines = Some(lines);
                        self.next = 0;
                    }
                    Err(error) => self.fallback(error),
                }
            }
        }
    }
    #[cfg(test)]
    pub fn response_for_test(&mut self, lines: Vec<Vec<Token>>) -> WorkerEvent {
        let version = self.desired.clone().unwrap();
        self.in_flight = Some(version.clone());
        self.status = PresentationStatus::Pending;
        WorkerEvent::Result(crate::syntax::SyntaxResponse {
            version,
            result: Ok(lines),
        })
    }

    pub fn next_line(&mut self) -> Option<HighlightStep> {
        let lines = self.lines.as_ref()?;
        if self.next >= lines.len() {
            self.status = PresentationStatus::Highlighted;
            return None;
        }
        let line = self.next;
        self.next += 1;
        let tokens = lines[line].clone();
        if self.next == lines.len() {
            self.status = PresentationStatus::Highlighted;
        }
        Some(HighlightStep { line, tokens })
    }
    pub fn has_work(&self) -> bool {
        self.status == PresentationStatus::Pending
            && (self.lines.is_some()
                || (self.in_flight.is_none() && self.worker.as_ref().is_none_or(|w| w.ready)))
    }
    fn fallback(&mut self, error: HighlightError) {
        self.stop();
        self.status = PresentationStatus::PlainFallback(error);
    }
    pub fn stop(&mut self) {
        self.worker = None;
        self.in_flight = None;
        self.lines = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocumentLimits, TextRange, language::TokenKind, syntax::SyntaxResponse};
    use wasm_bindgen_test::wasm_bindgen_test;
    fn document(text: &str) -> TextDocument {
        TextDocument::new(text, DocumentLimits::default()).unwrap()
    }
    fn cache() -> HighlightCache {
        let mut cache = HighlightCache::new();
        cache.settings = Some(syntax_worker::Settings {
            config: syntax_worker::LanguageWorkerConfig::new("http://localhost/app.js", "worker"),
            indents: [("rust".into(), "    ".into()), ("json".into(), "  ".into())].into(),
        });
        cache
    }
    #[wasm_bindgen_test]
    fn stale_revision_and_language_results_do_not_replace_newer_work() {
        let mut cache = cache();
        let doc = document("let x");
        cache.update(&doc, DocumentRevision(0), "rust", false);
        let old = cache.desired.clone().unwrap();
        cache.in_flight = Some(old.clone());
        cache.update(&doc, DocumentRevision(1), "json", false);
        cache.receive(
            WorkerEvent::Result(SyntaxResponse {
                version: old,
                result: Ok(vec![vec![]]),
            }),
            &doc,
        );
        assert!(cache.in_flight.is_none());
        assert!(cache.lines.is_none());
        assert_eq!(cache.status, PresentationStatus::Pending);
        let current = cache.desired.clone().unwrap();
        cache.in_flight = Some(current.clone());
        cache.receive(
            WorkerEvent::Result(SyntaxResponse {
                version: current,
                result: Ok(vec![vec![]]),
            }),
            &doc,
        );
        assert!(cache.next_line().is_some());
        assert_eq!(cache.status, PresentationStatus::Highlighted);
    }
    #[wasm_bindgen_test]
    fn loads_fence_results_even_at_the_same_revision_and_language() {
        let mut cache = cache();
        let doc = document("let x");
        cache.update(&doc, DocumentRevision(0), "rust", false);
        let old = cache.desired.clone().unwrap();
        cache.in_flight = Some(old.clone());
        cache.update(&doc, DocumentRevision(0), "rust", true);
        cache.receive(
            WorkerEvent::Result(SyntaxResponse {
                version: old,
                result: Err(HighlightError::TokenBudget),
            }),
            &doc,
        );
        assert_eq!(cache.status, PresentationStatus::Pending);
        assert!(cache.lines.is_none());
    }
    #[wasm_bindgen_test]
    fn invalid_worker_ranges_fall_back_and_span_overflow_is_sticky_until_reset() {
        let mut cache = cache();
        let doc = document("🦀");
        cache.update(&doc, DocumentRevision(0), "rust", false);
        let version = cache.desired.clone().unwrap();
        cache.in_flight = Some(version.clone());
        cache.receive(
            WorkerEvent::Result(SyntaxResponse {
                version,
                result: Ok(vec![vec![Token {
                    range: TextRange::new(0, 1),
                    kind: TokenKind::String,
                }]]),
            }),
            &doc,
        );
        assert_eq!(
            cache.status,
            PresentationStatus::PlainFallback(HighlightError::InvalidRange)
        );
        cache.fallback(HighlightError::TokenBudget);
        cache.update(&doc, DocumentRevision(1), "rust", false);
        assert_eq!(
            cache.status,
            PresentationStatus::PlainFallback(HighlightError::TokenBudget)
        );
        cache.update(&doc, DocumentRevision(2), "rust", true);
        assert_eq!(cache.status, PresentationStatus::Pending);
        cache.fallback(HighlightError::InvalidLanguage("bad query".into()));
        cache.update(&doc, DocumentRevision(3), "rust", false);
        assert!(matches!(
            cache.status,
            PresentationStatus::PlainFallback(HighlightError::InvalidLanguage(_))
        ));
        cache.update(&doc, DocumentRevision(4), "rust", true);
        assert_eq!(cache.status, PresentationStatus::Pending);
    }
}
