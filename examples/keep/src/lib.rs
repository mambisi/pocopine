//! Keep example — a Google-Keep-style sync demo.
//!
//! Code is split into:
//!
//! - [`KeepBoard`] — the only `#[component]` mounted in the DOM. It
//!   hosts the layout shell and, on mount, hooks the sync client up to
//!   the [`store::KeepStore`] singleton.
//! - [`store::KeepStore`] — `#[store]` singleton that owns durable app
//!   state: the synced `notes` collection, registered labels, sidebar
//!   and search state, command state, and sync handlers.
//! - [`components`] — UI components: board shell, composer shell,
//!   shared note form, note body editor, note cards, and editor modal.
//!   The shared form owns the active edit buffer locally and dispatches
//!   a typed save payload back into [`store::KeepStore`].

#[cfg(target_arch = "wasm32")]
mod app;
pub mod components;
pub mod firebase;
pub mod model;
#[cfg(pocopine_host)]
pub mod sqlite_stream;
pub mod store;
pub mod sync;
pub mod utils;

pub use components::{
    KeepAuthGate, KeepBoard, KeepComposer, KeepEditor, KeepGridLayout, KeepListDetail, KeepLogin,
    KeepNoteBody, KeepNoteCard, KeepNoteForm,
};
pub use firebase::{keep_firebase_auth_plugin, FirebaseAuthUser, KeepFirebaseAuth};
pub use model::*;
pub use store::KeepStore;
pub use sync::reset_keep_notes;
#[cfg(pocopine_host)]
pub use sync::{live_backend, sync_server};
pub use utils::{focus_after_flush, now_ms, shared_layout_transition};
