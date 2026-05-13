//! UI components for the Keep example.
//!
//! Long-lived app state lives in [`crate::KeepStore`]. Short-lived
//! editing buffers live inside their form components so the inline
//! composer and modal editor can share the same controls without
//! duplicating template branches.

pub mod board;
pub mod composer;
pub mod editor;
pub mod note_body;
pub mod note_card;
pub mod note_form;

pub use board::KeepBoard;
pub use composer::KeepComposer;
pub use editor::KeepEditor;
pub use note_body::KeepNoteBody;
pub use note_card::KeepNoteCard;
pub use note_form::KeepNoteForm;
