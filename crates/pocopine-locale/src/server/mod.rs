//! Host compilation and catalog preparation. Platform gating lives at the
//! crate root. Project discovery is the explicit IO boundary; catalog
//! compilation is deterministic and operates on supplied source records.

mod cfg;
mod compile;
mod extract_rust;
mod extract_template;
mod project;
mod source;

pub use cfg::CfgSet;
pub use compile::{
    CatalogSource, Compilation, CompiledCatalog, MessageReference, MessageSignature, ReferenceKind,
    compile_catalogs,
};
pub use extract_rust::{
    IncludeRequest, ModuleRequest, RustExtraction, TemplateRequest, extract_rust,
};
pub use extract_template::{Extraction, SourceContext, extract_template};
pub use pocopine_stylekit::{Diagnostic, Severity, Span};
pub use project::{ProjectDiscovery, SourceFile, SourceTarget, discover_project};
pub use source::{SourceMessage, SourceMessages, parse_messages};
