//! `<pine-stack>` — vertical one-dimensional layout (Atlassian's
//! `Stack`).
//!
//! Headless: emits `class="pine-stack"` plus `data-gap`/`data-align`/
//! `data-justify` hooks. The author's CSS supplies the flexbox:
//!
//! ```css
//! .pine-stack { display: flex; flex-direction: column; }
//! .pine-stack[data-gap="4"]        { gap: 1rem; }
//! .pine-stack[data-align="center"] { align-items: center; }
//! ```

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// Stacks its children vertically. See module docs.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineStack.poco", role = "panel")]
pub struct PineStack {
    /// Gap token between children. Surfaced as `data-gap`.
    #[prop]
    pub gap: String,
    /// Cross-axis (horizontal) alignment — e.g. `start`/`center`/
    /// `end`/`stretch`. Surfaced as `data-align`.
    #[prop]
    pub align: String,
    /// Main-axis (vertical) distribution — e.g. `start`/`center`/
    /// `between`. Surfaced as `data-justify`.
    #[prop]
    pub justify: String,
}

#[handlers]
impl PineStack {}
