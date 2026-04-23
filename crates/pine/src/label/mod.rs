//! `<pine-label>` — accessible text label tied to a form control.
//!
//! Mirrors Radix `<Label>`. Pairs with a form input via the
//! `target="<id>"` attribute (flows to the underlying
//! `<label>`'s `for`). Click-through works by default via
//! native `<label>` semantics.
//!
//! Named `target` rather than `for` because `for` collides with
//! Rust's keyword and with template-expression parsing; the
//! attribute still renders as a standard `for="…"` in the DOM.
//!
//! ```html
//! <pine-label target="name">Full name</pine-label>
//! <input id="name" type="text">
//! ```

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineLabel.poco", role = "label")]
pub struct PineLabel {
    /// `id` of the form control this label is paired with.
    /// Flows to the underlying `<label>`'s `for` attribute.
    #[prop]
    pub target: String,
}

#[handlers]
impl PineLabel {}
