//! Host compilation and catalog preparation. Platform gating lives at the
//! crate root; the compiler is deterministic and does not perform hidden IO.

mod compile;
mod source;

pub use compile::{
    CatalogSource, Compilation, CompiledCatalog, MessageReference, MessageSignature, ReferenceKind,
    compile_catalogs,
};
pub use pocopine_stylekit::{Diagnostic, Severity, Span};
pub use source::{SourceMessage, SourceMessages, parse_messages};
