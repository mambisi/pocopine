use pine_code::{DocumentLimits, TextDocument, TextRange, language::*};

fn tokenize(language: BuiltinLanguage, text: &str) -> TokenizedLine<LexState> {
    tokenize_line(
        &language,
        text,
        &language.start_state(),
        HighlightLimits::default(),
    )
    .unwrap()
}
fn doc(text: &str) -> TextDocument {
    TextDocument::new(text, DocumentLimits::default()).unwrap()
}

#[test]
fn json_handles_escapes_literals_numbers_and_incomplete_strings() {
    let source = r#"{"hello": "a\"🦀", "ok": true, "n": -1.2e+3, "x": null}"#;
    let line = tokenize(BuiltinLanguage::Json, source);
    assert!(
        line.tokens
            .iter()
            .any(|token| token.kind == TokenKind::String
                && &source[token.range.from.0..token.range.to.0] == r#""a\"🦀""#)
    );
    assert!(
        line.tokens
            .iter()
            .any(|token| token.kind == TokenKind::Number
                && &source[token.range.from.0..token.range.to.0] == "-1.2e+3")
    );
    assert_eq!(line.state, BuiltinLanguage::Json.start_state());
    let incomplete = tokenize(BuiltinLanguage::Json, "\"unfinished");
    assert_ne!(incomplete.state, BuiltinLanguage::Json.start_state());
    let completed = tokenize_line(
        &BuiltinLanguage::Json,
        "still string\"",
        &incomplete.state,
        HighlightLimits::default(),
    )
    .unwrap();
    assert_eq!(completed.state, BuiltinLanguage::Json.start_state());
    assert_ne!(incomplete.state, completed.state);
}

#[test]
fn rust_distinguishes_char_literals_lifetimes_and_raw_strings() {
    let source = "fn f<'a>(x: &'a str) { let a = '🦀'; let b = '\\u{1f980}'; let s = r##\"raw \"# text\"##; }";
    let line = tokenize(BuiltinLanguage::Rust, source);
    let kinds = line
        .tokens
        .iter()
        .map(|token| (token.kind, &source[token.range.from.0..token.range.to.0]))
        .collect::<Vec<_>>();
    assert!(kinds.contains(&(TokenKind::Keyword, "fn")));
    assert!(kinds.contains(&(TokenKind::Lifetime, "'a")));
    assert!(kinds.contains(&(TokenKind::String, "'🦀'")));
    assert!(kinds.contains(&(TokenKind::String, "'\\u{1f980}'")));
    assert!(kinds.contains(&(TokenKind::String, "r##\"raw \"# text\"##")));
    assert_eq!(line.state, BuiltinLanguage::Rust.start_state());
}

#[test]
fn nested_comments_cross_blank_lines_with_independent_state_snapshots() {
    let language = BuiltinLanguage::Rust;
    let initial = language.start_state();
    let first = tokenize_line(
        &language,
        "/* outer /* inner",
        &initial,
        HighlightLimits::default(),
    )
    .unwrap();
    let blank = tokenize_line(&language, "", &first.state, HighlightLimits::default()).unwrap();
    let last = tokenize_line(
        &language,
        "*/ still outer */ fn",
        &blank.state,
        HighlightLimits::default(),
    )
    .unwrap();
    assert_eq!(first.state, blank.state);
    assert_ne!(first.state, initial);
    assert_eq!(last.state, initial);
    assert_eq!(last.tokens.last().unwrap().kind, TokenKind::Keyword);
}

#[test]
fn cache_reuses_suffix_only_after_lexical_state_converges() {
    let mut cache = HighlightCache::new(HighlightLimits::default());
    cache.update(&doc("let a = 1;\nbody\n*/\nlet b = 2;"), "rust");
    while cache.next_line().unwrap().is_some() {}
    cache.update(&doc("/*\nbody\n*/\nlet b = 2;"), "rust");
    let mut lines = Vec::new();
    while let Some(step) = cache.next_line().unwrap() {
        lines.push(step.line);
    }
    assert_eq!(lines, [0, 1, 2]);
    cache.update(&doc("/*\nbody\n*/\nlet c = 2;"), "rust");
    assert_eq!(cache.next_line().unwrap().unwrap().line, 3);
    assert!(cache.next_line().unwrap().is_none());
}

#[test]
fn budgets_fall_back_before_lexing_long_lines_or_creating_excess_spans() {
    let mut cache = HighlightCache::new(HighlightLimits {
        max_spans: 2,
        max_line_bytes: 8,
        slice_ms: 4,
    });
    cache.update(&doc("\"123456789\""), "json");
    assert_eq!(
        cache.status,
        PresentationStatus::PlainFallback(HighlightError::LineBudget)
    );
    cache.update(&doc("true 1\nfalse 2"), "json");
    assert!(cache.next_line().unwrap().is_some());
    assert!(matches!(
        cache.next_line(),
        Err(HighlightError::TokenBudget)
    ));
    cache.update(&doc("true"), "json");
    assert_eq!(
        cache.status,
        PresentationStatus::PlainFallback(HighlightError::TokenBudget)
    );
    assert!(cache.next_line().unwrap().is_none());
    cache.invalidate();
    cache.update(&doc("true"), "json");
    assert!(cache.next_line().unwrap().is_some());
    assert_eq!(cache.status, PresentationStatus::Highlighted);
    cache.update(&doc("ok"), "unknown");
    assert!(matches!(
        cache.status,
        PresentationStatus::PlainFallback(HighlightError::UnknownLanguage(_))
    ));
}

struct Stuck;
impl Language for Stuck {
    type State = u32;
    fn start_state(&self) -> u32 {
        0
    }
    fn token(
        &self,
        _: &mut LineStream<'_>,
        state: &mut u32,
    ) -> Result<Option<TokenKind>, HighlightError> {
        *state += 1;
        Ok(None)
    }
    fn blank_line(&self, state: &mut u32) {
        *state += 1;
    }
}

#[test]
fn driver_bounds_nonadvancing_tokenizers_and_validates_utf8_ranges() {
    assert_eq!(
        tokenize_line(&Stuck, "text", &0, HighlightLimits::default()).unwrap_err(),
        HighlightError::NoProgress
    );
    assert_eq!(
        tokenize_line(&Stuck, "", &0, HighlightLimits::default())
            .unwrap()
            .state,
        1
    );
    assert_eq!(
        validate_tokens(
            "🦀",
            &[Token {
                range: TextRange::new(0, 1),
                kind: TokenKind::String
            }]
        ),
        Err(HighlightError::InvalidRange)
    );
    assert_eq!(
        validate_tokens(
            "abc",
            &[
                Token {
                    range: TextRange::new(1, 3),
                    kind: TokenKind::String
                },
                Token {
                    range: TextRange::new(2, 3),
                    kind: TokenKind::Comment
                }
            ]
        ),
        Err(HighlightError::InvalidRange)
    );
}
