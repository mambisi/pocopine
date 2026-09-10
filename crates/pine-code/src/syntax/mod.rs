//! Registered Tree-sitter grammars and bounded, incremental highlighting.
//! Parsing is independent of editing and runs in a dedicated worker in the view.
pub mod languages;
mod parser;
mod registry;
pub use parser::SyntaxParser;
pub use registry::{LanguageError, LanguageRegistry, TreeSitterLanguage};

use crate::language::{HighlightError, HighlightLimits, Token};
use serde::{Deserialize, Serialize};

/// The generation also fences loads and language switches at an unchanged revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntaxVersion {
    pub generation: u64,
    pub revision: crate::DocumentRevision,
    pub language: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyntaxRequest {
    pub version: SyntaxVersion,
    pub text: String,
    pub limits: HighlightLimits,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyntaxResponse {
    pub version: SyntaxVersion,
    pub result: Result<Vec<Vec<Token>>, HighlightError>,
}
