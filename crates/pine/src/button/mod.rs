//! `PineButton` — polymorphic button primitive.
//!
//! Accepts `variant` / `size` / `disabled` props, renders as a
//! native `<button type="button">`. Use `pp-as` on the tag to
//! render the template's attrs onto an `<a>` / router-link / any
//! other element instead — useful for "button that's actually a
//! link" cases where keyboard + semantic behaviour should match
//! the rendered element.
//!
//! ```html
//! <pine-button variant="primary" @click="save">Save</pine-button>
//!
//! <!-- Render as <a> — click semantics + keyboard come from <a>. -->
//! <pine-button pp-as variant="ghost">
//!   <a pp-route href="/docs">Docs</a>
//! </pine-button>
//! ```

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// Button primitive. All props are `data-*` attributes on the
/// rendered element — Pine ships no styling. Authors style via
/// `.pine-btn[data-variant="primary"] { … }`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineButton.poco", role = "interactive")]
pub struct PineButton {
    /// Arbitrary variant name — becomes `data-variant`. No
    /// built-in values; whatever the author writes gets rendered.
    #[prop] pub variant: String,
    /// Size variant — becomes `data-size`.
    #[prop] pub size: String,
    /// When true, renders the `disabled` attribute on the inner
    /// `<button>` (blocking native click) and `data-disabled`
    /// for CSS hooks.
    #[prop] pub disabled: bool,
}

#[handlers]
impl PineButton {}
