//! `<pine-form-root>` — native form wrapper that coordinates
//! a group of `<pine-field-root>` descendants.
//!
//! Mirrors Base UI's `<Form>`. One part:
//!
//! - **Root** (`pine-form-root`) — renders a native `<form>`,
//!   intercepts submit (`preventDefault`), and re-dispatches as
//!   the Pine-native `pp:submit` event so parents can bind with
//!   `@submit="handler"` without fighting the browser's default
//!   navigation.
//!
//! Provides its Handle so descendant Fields can subscribe to
//! the `errors` prop. When `errors` carries a key matching a
//! Field's `name`, that Field flips to `invalid = true` — the
//! canonical shape for server / external validation feedback.
//!
//! ```html
//! <pine-form-root :errors="server_errors" @submit="submit">
//!   <pine-field-root name="email">
//!     <pine-field-label>Email</pine-field-label>
//!     <pine-field-control><input type="email" required></pine-field-control>
//!     <pine-field-error>Please enter a valid email.</pine-field-error>
//!   </pine-field-root>
//!   <button type="submit">Save</button>
//! </pine-form-root>
//! ```
//!
//! Values flow from inputs through `pp-model:value` into parent
//! state — the `@submit` handler reads that state directly, so
//! Pine doesn't need to ship a FormData parser.

use pocopine::prelude::*;
use pocopine::{emit, inject_key, provide};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

inject_key!(pub FORM: Handle<PineFormRoot>);

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineFormRoot.poco", role = "scope")]
pub struct PineFormRoot {
    /// External validation errors keyed by field `name`. Fields
    /// whose `name` appears as a key flip `invalid = true`; the
    /// rest clear their invalid flag. Typically written after a
    /// server-side submit round-trip.
    #[prop] pub errors: HashMap<String, String>,
}

#[handlers]
impl PineFormRoot {
    pub fn on_setup(&mut self) {
        provide(&FORM, this::<Self>());
    }

    /// `@submit.prevent` runs this; we then re-emit the submit
    /// as a bubbling `pp:submit` for the parent's `@submit`
    /// binding. Values live in the parent's state via
    /// `pp-model:value` bindings on the inputs — no payload here.
    pub fn on_submit(&mut self) {
        emit("pp:submit", ());
    }
}
