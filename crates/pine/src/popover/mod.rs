//! `PinePopover` — anchored floating element (placeholder).

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PinePopover.poco")]
pub struct PinePopover {}

#[handlers]
impl PinePopover {}
