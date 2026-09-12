#![cfg(all(
    feature = "lang-rust",
    feature = "lang-json",
    feature = "lang-python",
    feature = "lang-javascript"
))]
use pine_code::{
    LanguageError, LanguageRegistry, TreeSitterLanguage,
    language::{HighlightError, HighlightLimits, Token, TokenKind, validate_tokens},
    languages,
    syntax::SyntaxParser,
};
fn registry() -> LanguageRegistry {
    let mut languages = LanguageRegistry::new();
    for language in [
        languages::rust(),
        languages::json(),
        languages::python(),
        languages::javascript(),
    ] {
        languages.register(language).unwrap();
    }
    languages
}
fn parser() -> SyntaxParser {
    SyntaxParser::new(registry())
}
fn contains(source: &str, lines: &[Vec<Token>], text: &str, kind: TokenKind) -> bool {
    source.split('\n').zip(lines).any(|(line, tokens)| {
        tokens
            .iter()
            .any(|t| t.kind == kind && &line[t.range.from.0..t.range.to.0] == text)
    })
}
#[test]
fn registers_custom_grammar_and_rejects_ambiguous_metadata() {
    let mut registry = registry();
    assert!(matches!(
        registry.register(languages::rust()),
        Err(LanguageError::DuplicateId(_))
    ));
    assert!(matches!(
        registry.register(
            TreeSitterLanguage::new("plain", tree_sitter_json::LANGUAGE)
                .highlights("(number) @number")
        ),
        Err(LanguageError::InvalidId(_))
    ));
    assert!(matches!(
        registry.register(
            TreeSitterLanguage::new("custom", tree_sitter_json::LANGUAGE)
                .highlights("(number) @number")
                .indent_unit("x")
        ),
        Err(LanguageError::InvalidIndent(_))
    ));
    registry
        .register(
            TreeSitterLanguage::new("config", tree_sitter_json::LANGUAGE)
                .highlights("(number) @keyword"),
        )
        .unwrap();
    let source = "{\"count\": 42}";
    let result = SyntaxParser::new(registry)
        .highlight(source, "config", HighlightLimits::default())
        .unwrap();
    assert!(contains(source, &result, "42", TokenKind::Keyword));
}
#[test]
fn grammars_distinguish_context_and_incomplete_code() {
    let mut parser = parser();
    let limits = HighlightLimits::default();
    let result = parser.highlight("let fu", "rust", limits).unwrap();
    assert!(contains("let fu", &result, "let", TokenKind::Keyword));
    assert!(!contains("let fu", &result, "fu", TokenKind::Keyword));
    for (language, source, cases) in [
        (
            "rust",
            "fn greet(value: i32) -> String { println!(\"hi\"); value.to_string() }",
            vec![
                ("greet", TokenKind::Function),
                ("i32", TokenKind::Type),
                ("String", TokenKind::Type),
            ],
        ),
        (
            "json",
            "{\"café😀\": \"value\", \"n\": 42}",
            vec![
                ("\"café😀\"", TokenKind::Property),
                ("\"value\"", TokenKind::String),
                ("42", TokenKind::Number),
            ],
        ),
        (
            "python",
            "def greet(name):\n    return name + \"hello\"",
            vec![
                ("def", TokenKind::Keyword),
                ("greet", TokenKind::Function),
                ("return", TokenKind::Keyword),
            ],
        ),
        (
            "javascript",
            "function greet(name) { return name + 42; }",
            vec![
                ("function", TokenKind::Keyword),
                ("greet", TokenKind::Function),
                ("42", TokenKind::Number),
            ],
        ),
    ] {
        let result = parser.highlight(source, language, limits).unwrap();
        for (text, kind) in cases {
            assert!(
                contains(source, &result, text, kind),
                "{language}: {text}: {result:?}"
            );
        }
        for (line, tokens) in source.split('\n').zip(&result) {
            validate_tokens(line, tokens).unwrap();
        }
    }
}
#[test]
fn unicode_multiline_edits_and_recovery_match_fresh_highlighting() {
    let mut parser = parser();
    for source in [
        "fn main() { let café = \"😀\"; }\n",
        "fn main() { let result\n = \"😀\"; }\n",
        "/* fn main() { let café = \"😀\"; }\n",
        "/* outer\n/* inner */\n*/ fn main() {}",
        "fn main() { let s = r###\"fn 😀\"###; }",
        "",
        "fn main() {}",
        "fn main() {}",
    ] {
        let result = parser
            .highlight(source, "rust", HighlightLimits::default())
            .unwrap();
        let fresh = SyntaxParser::new(registry())
            .highlight(source, "rust", HighlightLimits::default())
            .unwrap();
        assert_eq!(result, fresh, "{source}");
    }
}
#[test]
fn budgets_and_invalid_queries_do_not_poison_subsequent_requests() {
    let mut registry = registry();
    registry
        .register(
            TreeSitterLanguage::new("broken", tree_sitter_json::LANGUAGE)
                .highlights("(nonexistent) @keyword"),
        )
        .unwrap();
    let mut parser = SyntaxParser::new(registry);
    assert!(matches!(
        parser.highlight("{}", "broken", HighlightLimits::default()),
        Err(HighlightError::InvalidLanguage(_))
    ));
    assert!(matches!(
        parser.highlight("{}", "unknown", HighlightLimits::default()),
        Err(HighlightError::UnknownLanguage(_))
    ));
    assert_eq!(
        parser.highlight(&"x".repeat(9000), "rust", HighlightLimits::default()),
        Err(HighlightError::LineBudget)
    );
    assert_eq!(
        parser.highlight(
            "fn main() {}",
            "rust",
            HighlightLimits {
                max_spans: 1,
                ..Default::default()
            }
        ),
        Err(HighlightError::TokenBudget)
    );
    assert!(
        parser
            .highlight("fn main() {}", "rust", HighlightLimits::default())
            .is_ok()
    );
}
