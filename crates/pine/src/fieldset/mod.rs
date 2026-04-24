//! `<pine-fieldset-*>` — grouping wrapper for related form
//! controls with a shared, stylable legend. Two parts:
//!
//! - **Root** (`pine-fieldset-root`) — native `<fieldset>` with
//!   `disabled` forwarded onto the attribute (which natively
//!   disables every descendant form control). Generates a
//!   `legend_id` and wires `aria-labelledby` so the Legend part
//!   is recognised as the accessible label even though it's not
//!   a native `<legend>`.
//! - **Legend** (`pine-fieldset-legend`) — `<div>` carrying the
//!   shared id. Renders as a `<div>` rather than `<legend>` so
//!   authors can style it freely (native `<legend>` has awkward
//!   layout behaviour around the fieldset's border).
//!
//! ```html
//! <pine-fieldset-root>
//!   <pine-fieldset-legend>Account</pine-fieldset-legend>
//!   <pine-field-root name="username">
//!     <pine-field-label>Username</pine-field-label>
//!     <pine-field-control><input type="text"></pine-field-control>
//!   </pine-field-root>
//!   <pine-field-root name="password">
//!     <pine-field-label>Password</pine-field-label>
//!     <pine-field-control><input type="password"></pine-field-control>
//!   </pine-field-root>
//! </pine-fieldset-root>
//! ```

use pocopine::prelude::*;
use pocopine::{current_scope_id, inject_key, provide};
use serde::{Deserialize, Serialize};

inject_key!(ROOT: Handle<PineFieldsetRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineFieldsetRoot.poco", role = "panel")]
// RFC 049 — a Fieldset groups form controls under a Legend.
// `accepts` so authors can drop Input / Label / custom
// controls alongside; the Legend is the only semantic part
// pocopine contributes here.
#[slot(default, accepts = [PineFieldsetLegend])]
pub struct PineFieldsetRoot {
    /// Forwarded onto the `<fieldset>`'s native `disabled`
    /// attribute — which disables every descendant form control
    /// automatically, so there's no programmatic cascade.
    #[prop]
    pub disabled: bool,

    /// Id the Legend adopts so the Root can reference it via
    /// `aria-labelledby`. Generated from the Root's scope id.
    pub legend_id: String,
}

#[handlers]
impl PineFieldsetRoot {
    pub fn on_setup(&mut self) {
        if let Some(scope) = current_scope_id() {
            self.legend_id = format!("pine-fieldset-legend-{}", scope.0);
        }
        provide(&ROOT, this::<Self>());
    }
}

// ── Legend ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineFieldsetLegend.poco", role = "heading")]
pub struct PineFieldsetLegend {
    #[observe(ROOT)]
    pub legend_id: String,
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineFieldsetLegend {}
