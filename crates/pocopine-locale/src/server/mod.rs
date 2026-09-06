//! Host compilation and catalog preparation. Platform gating lives at the
//! crate root. Project discovery is the explicit IO boundary; catalog
//! compilation is deterministic and operates on supplied source records.

mod catalogs;
mod cfg;
mod codegen;
mod compile;
mod extract_rust;
mod extract_template;
mod preload;
mod project;
mod source;
mod xliff;

pub use catalogs::ServerCatalogs;
pub use cfg::CfgSet;
pub use codegen::{generate_rust, generate_rust_with_config};
pub use compile::{
    CatalogSource, Compilation, CompiledCatalog, MessageReference, MessageSignature, ReferenceKind,
    compile_catalogs,
};
pub use extract_rust::{
    IncludeRequest, ModuleRequest, RustExtraction, TemplateRequest, extract_rust,
};
pub use extract_template::{Extraction, SourceContext, extract_template};
pub use pocopine_stylekit::{Diagnostic, Severity, Span};
pub use preload::{locale_directions, preload_fallbacks};
pub use project::{
    DiscoveryOptions, ProjectDiscovery, SourceFile, SourceTarget, discover_project,
    discover_project_with_options,
};
pub use source::{SourceMessage, SourceMessages, parse_messages};
pub use xliff::{XliffDocument, XliffUnit, export_xliff, import_xliff};

mod data;
pub use data::{formatting_data, plural_data};
