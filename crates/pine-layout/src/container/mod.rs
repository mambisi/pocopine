//! `<pine-container>` — a max-width content column.
//!
//! Headless: emits `class="pine-container"` plus `data-size` and
//! `data-safe-area` hooks. The author's CSS (or Stylekit utilities)
//! decides the actual max-widths and whether to center:
//!
//! ```css
//! .pine-container { margin-inline: auto; width: 100%; }
//! .pine-container[data-size="md"]  { max-width: 48rem; }
//! .pine-container[data-size="lg"]  { max-width: 64rem; }
//! .pine-container[data-safe-area]  { padding: env(safe-area-inset-top)
//!                                            env(safe-area-inset-right)
//!                                            env(safe-area-inset-bottom)
//!                                            env(safe-area-inset-left); }
//! ```

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// A centered, max-width content column. See module docs.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineContainer.poco", role = "panel")]
pub struct PineContainer {
    /// Max-width token — e.g. `sm`/`md`/`lg`/`xl`/`2xl`/`full`, or any
    /// author-defined value. Surfaced as `data-size`.
    #[prop]
    pub size: String,
    /// Opt into safe-area inset padding. Surfaced as the boolean
    /// `data-safe-area` attribute (present when `true`).
    #[prop]
    pub safe_area: bool,
}

#[handlers]
impl PineContainer {}
