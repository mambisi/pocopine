//! `<pine-progress-*>` — accessible progress bar (compound).
//!
//! Mirrors Radix `<Progress>`:
//!
//! - **Root** (`pine-progress-root`) — `role="progressbar"`,
//!   tracks `value` and `max`, recomputes a clamped `percent`
//!   whenever either changes, and stamps `data-state` +
//!   `data-value` + `data-max` for authors to style against.
//! - **Indicator** (`pine-progress-indicator`) — mirrors
//!   `percent` from Root and renders `style="width:<percent>%"`
//!   so a fill bar works out-of-the-box. CSS authors can still
//!   override (transform-based animations, vertical bars, etc.)
//!   by setting their own `width`.
//!
//! A negative `value` signals **indeterminate** — the
//! `data-state` flips to `"indeterminate"`, `data-value` +
//! `aria-valuenow` are omitted, and `percent` falls back to 0 so
//! the indicator's intrinsic width disappears and the CSS author
//! can drop in a loading animation.
//!
//! ```html
//! <pine-progress-root value="42" max="100">
//!   <pine-progress-indicator></pine-progress-indicator>
//! </pine-progress-root>
//! ```

use pocopine::create_context;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

create_context!(ROOT: Handle<PineProgressRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineProgressRoot.poco", role = "panel")]
// RFC 049 — Progress holds exactly one Indicator. The
// indicator's width / transform draws the filled portion;
// nothing else belongs at Root level (a label belongs outside
// the progress's a11y surface).
#[slot(default, only = [PineProgressIndicator])]
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
    /// Clamped `[0..100]` percentage derived from `value`/`max`.
    /// `0` while indeterminate or when `max <= 0`. Mirrored onto
    /// the Indicator via `#[observe(ROOT)]` so templates can bind
    /// `:style="'width:' + percent + '%'"` without doing math in
    /// the expression layer (pine-expr keeps templates simple —
    /// arithmetic lives in Rust).
    pub percent: f64,
}

impl Default for PineProgressRoot {
    fn default() -> Self {
        Self {
            value: 0.0,
            max: 100.0,
            percent: 0.0,
        }
    }
}

#[handlers]
impl PineProgressRoot {
    fn on_setup(&mut self) {
        self.recompute_percent();
        ROOT.provide(this::<Self>());
    }

    #[watch(value)]
    fn on_value(&mut self, _: f64, _: Option<f64>) {
        self.recompute_percent();
    }

    #[watch(max)]
    fn on_max(&mut self, _: f64, _: Option<f64>) {
        self.recompute_percent();
    }
}

impl PineProgressRoot {
    fn recompute_percent(&mut self) {
        if self.value < 0.0 || self.max <= 0.0 {
            self.percent = 0.0;
            return;
        }
        let pct = (self.value / self.max) * 100.0;
        self.percent = pct.clamp(0.0, 100.0);
    }
}

// ── Indicator ─────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineProgressIndicator.poco", role = "panel")]
pub struct PineProgressIndicator {
    /// Mirrored from Root for `data-value` + `data-state`.
    #[observe(ROOT)]
    pub value: f64,
    #[observe(ROOT)]
    pub max: f64,
    /// Mirrored from Root for the `width:<percent>%` style.
    #[observe(ROOT)]
    pub percent: f64,
}

#[handlers]
impl PineProgressIndicator {}
