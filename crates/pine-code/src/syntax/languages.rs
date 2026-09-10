//! Optional language packages. Each Cargo feature includes only its own grammar.
#[cfg(any(
    feature = "lang-rust",
    feature = "lang-json",
    feature = "lang-python",
    feature = "lang-javascript"
))]
use super::TreeSitterLanguage;

#[cfg(feature = "lang-rust")]
pub fn rust() -> TreeSitterLanguage {
    TreeSitterLanguage::new("rust", tree_sitter_rust::LANGUAGE)
        .highlights(tree_sitter_rust::HIGHLIGHTS_QUERY)
        .indent_unit("    ")
}
#[cfg(feature = "lang-json")]
pub fn json() -> TreeSitterLanguage {
    // 0.27 resolves later patterns last; upstream's earlier key pattern is
    // otherwise overridden by its generic string pattern.
    TreeSitterLanguage::new("json", tree_sitter_json::LANGUAGE).highlights(format!(
        "{}\n(pair key: (string) @string.special.key)\n",
        tree_sitter_json::HIGHLIGHTS_QUERY
    ))
}
#[cfg(feature = "lang-python")]
pub fn python() -> TreeSitterLanguage {
    TreeSitterLanguage::new("python", tree_sitter_python::LANGUAGE)
        .highlights(tree_sitter_python::HIGHLIGHTS_QUERY)
        .indent_unit("    ")
}
#[cfg(feature = "lang-javascript")]
pub fn javascript() -> TreeSitterLanguage {
    TreeSitterLanguage::new("javascript", tree_sitter_javascript::LANGUAGE)
        .highlights(tree_sitter_javascript::HIGHLIGHT_QUERY)
}
