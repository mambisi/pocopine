//! `<pine-aspect-ratio>` — fixed-ratio box.
//!
//! Mirrors Radix `<AspectRatio>`. Reserves a region of the layout
//! in a specific width-to-height ratio regardless of the content
//! inside. Uses modern CSS `aspect-ratio`; the default is 1:1
//! (square).
//!
//! ```html
//! <pine-aspect-ratio ratio="1.777">
//!   <img src="hero.jpg" alt="" style="width:100%;height:100%;object-fit:cover">
//! </pine-aspect-ratio>
//! ```

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(template = "PineAspectRatio.poco", role = "panel")]
pub struct PineAspectRatio {
    /// Width / height ratio. Examples: `1.0` (square), `1.777`
    /// (16:9), `0.5625` (9:16 vertical video).
    #[prop]
    pub ratio: f64,
}

impl Default for PineAspectRatio {
    fn default() -> Self {
        Self { ratio: 1.0 }
    }
}

#[handlers]
impl PineAspectRatio {}
