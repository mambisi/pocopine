//! Native code editing with checked UTF-8 positions and one transaction owner.
//!
//! The default feature set is target-independent. The optional `view` feature
//! exposes the Pocopine component on WASM. See RFC-124 for the input contract.

mod change;
pub mod commands;
mod error;
mod history;
pub mod language;
pub mod search;
mod state;
mod text;

pub use change::{Bias, Change, ChangeSet};
pub use error::{CodeError, CodeResult, CommitOutcome, ViewStatus};
pub use history::HistoryLimits;
pub use state::{CodeChange, EditOrigin, Editor, EditorState, Transaction};
pub use text::{
    DocumentLimits, DocumentRevision, LineEnding, Selection, TextDocument, TextOffset, TextRange,
    byte_to_utf16, normalize_lf, utf16_to_byte,
};

#[cfg(all(feature = "view", target_arch = "wasm32"))]
pub mod client;
