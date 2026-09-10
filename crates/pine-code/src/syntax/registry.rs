use std::collections::BTreeMap;
use std::fmt;
use tree_sitter_language::LanguageFn;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LanguageError {
    InvalidId(String),
    DuplicateId(String),
    InvalidIndent(String),
    MissingHighlights(String),
    IncompatibleGrammar(String),
}
impl fmt::Display for LanguageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for LanguageError {}

/// A grammar and queries compiled into the application's worker module.
/// Queries use Tree-sitter captures, e.g. `@function`, `@string.special.key`.
#[derive(Clone)]
pub struct TreeSitterLanguage {
    pub(crate) id: String,
    pub(crate) grammar: LanguageFn,
    pub(crate) highlights: String,
    pub(crate) indent: String,
}
impl TreeSitterLanguage {
    pub fn new(id: impl Into<String>, grammar: LanguageFn) -> Self {
        Self {
            id: id.into(),
            grammar,
            highlights: String::new(),
            indent: "  ".into(),
        }
    }
    pub fn highlights(mut self, query: impl Into<String>) -> Self {
        self.highlights = query.into();
        self
    }
    pub fn indent_unit(mut self, indent: impl Into<String>) -> Self {
        self.indent = indent.into();
        self
    }
    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Clone, Default)]
pub struct LanguageRegistry {
    pub(crate) entries: BTreeMap<String, TreeSitterLanguage>,
}
impl LanguageRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    /// Validates registration metadata. Query compilation runs lazily in the
    /// parser worker; invalid queries report `HighlightError::InvalidLanguage`.
    pub fn register(&mut self, language: TreeSitterLanguage) -> Result<(), LanguageError> {
        let id = &language.id;
        if id.is_empty()
            || id.len() > 64
            || matches!(id.as_str(), "plain" | "text")
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
        {
            return Err(LanguageError::InvalidId(id.clone()));
        }
        if self.entries.contains_key(id) {
            return Err(LanguageError::DuplicateId(id.clone()));
        }
        if language.indent != "\t"
            && (language.indent.is_empty()
                || language.indent.len() > 32
                || !language.indent.bytes().all(|c| c == b' '))
        {
            return Err(LanguageError::InvalidIndent(id.clone()));
        }
        if language.highlights.trim().is_empty() {
            return Err(LanguageError::MissingHighlights(id.clone()));
        }
        let version = tree_sitter::Language::new(language.grammar).abi_version();
        if !(tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION)
            .contains(&version)
        {
            return Err(LanguageError::IncompatibleGrammar(id.clone()));
        }
        self.entries.insert(id.clone(), language);
        Ok(())
    }
    pub fn get(&self, id: &str) -> Option<&TreeSitterLanguage> {
        self.entries.get(id)
    }
    pub fn iter(&self) -> impl Iterator<Item = &TreeSitterLanguage> {
        self.entries.values()
    }
}
