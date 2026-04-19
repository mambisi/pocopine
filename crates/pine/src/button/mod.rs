//! `PineButton` — polymorphic button primitive (placeholder).

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineButton.poco")]
pub struct PineButton {}

#[handlers]
impl PineButton {}
