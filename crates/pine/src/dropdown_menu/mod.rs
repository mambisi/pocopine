//! `PineDropdownMenu` — menu overlay (placeholder).

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenu.poco")]
pub struct PineDropdownMenu {}

#[handlers]
impl PineDropdownMenu {}
