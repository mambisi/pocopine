//! `PineTooltip` — hover/focus tooltip primitive (placeholder).

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTooltip.poco")]
pub struct PineTooltip {}

#[handlers]
impl PineTooltip {}
