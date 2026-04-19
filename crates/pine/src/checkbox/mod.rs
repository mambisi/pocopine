//! `PineCheckbox` — tri-state checkbox primitive (placeholder).

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCheckbox.poco")]
pub struct PineCheckbox {}

#[handlers]
impl PineCheckbox {}
