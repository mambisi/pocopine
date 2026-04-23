//! `<pine-progress-*>` — accessible progress bar (compound).
//!
//! Mirrors Radix `<Progress>`:
//!
//! - **Root** (`pine-progress-root`) — `role="progressbar"`,
//!   tracks `value` and `max`, stamps `data-state` +
//!   `data-value` + `data-max` for authors to style against.
//! - **Indicator** (`pine-progress-indicator`) — mirrors those
//!   data attributes from Root so CSS on the indicator can
//!   transform / resize without needing the value plumbed in
//!   twice.
//!
//! A negative `value` signals **indeterminate** — the
//! `data-state` flips to `"indeterminate"` and both
//! `data-value` and `aria-valuenow` are omitted, matching
//! Radix's conventions for a loading spinner-like state.
//!
//! ```html
//! <pine-progress-root value="42" max="100">
//!   <pine-progress-indicator></pine-progress-indicator>
//! </pine-progress-root>
//! ```

use pocopine::prelude::*;
use pocopine::{inject_key, provide};
use serde::{Deserialize, Serialize};

inject_key!(ROOT: Handle<PineProgressRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineProgressRoot.poco", role = "panel")]
pub struct PineProgressRoot {
    /// Current progress. Negative (e.g. `-1`) marks an
    /// indeterminate state — spinner-style loading with no known
    /// percentage.
    #[prop]
    pub value: f64,
    /// Maximum value. Defaults to 100 so authors can pass a
    /// percentage directly.
    #[prop]
    pub max: f64,
}

impl Default for PineProgressRoot {
    fn default() -> Self {
        Self {
            value: 0.0,
            max: 100.0,
        }
    }
}

#[handlers]
impl PineProgressRoot {
    pub fn on_setup(&mut self) {
        provide(&ROOT, this::<Self>());
    }
}

// ── Indicator ─────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineProgressIndicator.poco", role = "panel")]
pub struct PineProgressIndicator {
    /// Mirrored from Root for template bindings. Authors style
    /// the indicator's transform/width with CSS reading
    /// `data-value` / `data-max`.
    #[observe(ROOT)]
    pub value: f64,
    #[observe(ROOT)]
    pub max: f64,
}

#[handlers]
impl PineProgressIndicator {}
