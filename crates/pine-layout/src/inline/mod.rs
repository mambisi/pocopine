//! `<pine-inline>` — horizontal one-dimensional layout (Atlassian's
//! `Inline`).
//!
//! Headless: emits `class="pine-inline"` plus `data-gap`/`data-align`/
//! `data-justify`/`data-wrap` hooks. The author's CSS supplies the
//! flexbox:
//!
//! ```css
//! .pine-inline { display: flex; flex-direction: row; align-items: center; }
//! .pine-inline[data-wrap]          { flex-wrap: wrap; }
//! .pine-inline[data-justify="between"] { justify-content: space-between; }
//! ```

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// Arranges its children horizontally. See module docs.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineInline.poco", role = "panel")]
pub struct PineInline {
    /// Gap token between children. Surfaced as `data-gap`.
    #[prop]
    pub gap: String,
    /// Cross-axis (vertical) alignment — e.g. `start`/`center`/`end`/
    /// `baseline`. Surfaced as `data-align`.
    #[prop]
    pub align: String,
    /// Main-axis (horizontal) distribution — e.g. `start`/`center`/
    /// `between`. Surfaced as `data-justify`.
    #[prop]
    pub justify: String,
    /// Allow children to wrap onto multiple rows. Surfaced as the
    /// boolean `data-wrap` attribute.
    #[prop]
    pub wrap: bool,
}

#[handlers]
impl PineInline {}
