//! `<pine-grid>` + `<pine-grid-item>` — a 12-column responsive grid
//! (the cross-system column-count consensus: Atlassian's 12, Material
//! and Bootstrap/Tailwind all 12-friendly).
//!
//! Headless: the grid emits `class="pine-grid"` + `data-cols`/
//! `data-gap`; items emit `class="pine-grid-item"` + `data-span`/
//! `data-start`. The author's CSS supplies the grid. Responsive column
//! counts are best expressed with Stylekit utilities directly on the
//! element (`<pine-grid class="grid-cols-12 md:grid-cols-6">`) or with
//! media queries against the `data-*` hooks:
//!
//! ```css
//! .pine-grid { display: grid; }
//! .pine-grid[data-cols="12"] { grid-template-columns: repeat(12, 1fr); }
//! .pine-grid[data-gap="4"]   { gap: 1rem; }
//! .pine-grid-item[data-span="6"]  { grid-column: span 6; }
//! .pine-grid-item[data-start="4"] { grid-column-start: 4; }
//! ```

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// A grid container. See module docs.
#[derive(Serialize, Deserialize)]
#[component(template = "PineGrid.poco", role = "panel")]
pub struct PineGrid {
    /// Number of columns. Defaults to `12`. Surfaced as `data-cols`.
    #[prop]
    pub cols: String,
    /// Gap token between cells. Surfaced as `data-gap`.
    #[prop]
    pub gap: String,
}

impl Default for PineGrid {
    fn default() -> Self {
        Self {
            cols: "12".into(),
            gap: String::new(),
        }
    }
}

#[handlers]
impl PineGrid {}

/// A single cell in a [`PineGrid`]. See module docs.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineGridItem.poco", role = "panel")]
pub struct PineGridItem {
    /// Number of columns this cell spans. Surfaced as `data-span`.
    #[prop]
    pub span: String,
    /// 1-based column the cell starts at. Surfaced as `data-start`.
    #[prop]
    pub start: String,
}

#[handlers]
impl PineGridItem {}
