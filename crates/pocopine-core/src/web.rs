//! Curated browser-API surface for Pine compounds.
//!
//! Re-exports the `web_sys` types Pine primitives reach for most
//! often, so authors don't have to maintain their own `web-sys`
//! `features = [...]` list every time they touch a new browser
//! API. Import from here:
//!
//! ```ignore
//! use pocopine_core::web::{DragEvent, DataTransfer, FileList};
//! ```
//!
//! Anything not in this list either (a) belongs in `pocopine-core`
//! proper because it's load-bearing for the framework, or (b) is
//! niche enough that the consuming crate should add its own
//! `web-sys` feature.
//!
//! ## Handler-typed vs read-only events
//!
//! The types in [`handler_events`] implement
//! [`crate::handler::FromHandlerArg`] and can appear directly as a
//! parameter on a `#[handlers]` method
//! (e.g. `fn on_click(&mut self, ev: MouseEvent)`). The types in
//! [`read_only_events`] are re-exported for handlers that take the
//! `Event` supertype and downcast, or for code that consumes the
//! event outside handler dispatch — typing a handler parameter
//! against one of those directly produces a
//! `trait FromHandlerArg is not implemented` compile error.
//!
//! The two groups are exposed as nested modules (not just doc
//! sections) so the distinction is enforceable by `rustfmt` — a
//! single `pub use` list gets sorted alphabetically and loses the
//! grouping.

/// Events that implement [`crate::handler::FromHandlerArg`] and
/// can appear directly as a `#[handlers]` method parameter.
pub mod handler_events {
    pub use web_sys::{
        ClipboardEvent, CustomEvent, DragEvent, Event, FocusEvent, InputEvent, KeyboardEvent,
        MouseEvent, PointerEvent, SubmitEvent, TouchEvent, UiEvent, WheelEvent,
    };
}

/// Events NOT in the `FromHandlerArg` impl list — accept via the
/// `Event` supertype + downcast, or consume outside handler dispatch.
pub mod read_only_events {
    pub use web_sys::{
        AnimationEvent, CompositionEvent, ErrorEvent, HashChangeEvent, MessageEvent,
        PageTransitionEvent, PopStateEvent, ProgressEvent, StorageEvent, TransitionEvent,
    };
}

// Re-export both event groups at the module root so authors can
// `use pocopine_core::web::DragEvent` directly without remembering
// which subgroup it sits in. The grouping above is documentation;
// the flat surface here is the ergonomic path.
pub use handler_events::*;
pub use read_only_events::*;

// Drag-and-drop + file flows.
pub use web_sys::{Blob, DataTransfer, File, FileList, FormData};

// DOM essentials.
pub use web_sys::{
    Document, Element, EventTarget, HtmlAnchorElement, HtmlButtonElement, HtmlElement,
    HtmlFormElement, HtmlImageElement, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement,
    Node, Url, Window,
};
