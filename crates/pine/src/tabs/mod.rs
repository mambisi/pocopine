//! `PineTabs` — tablist primitive (placeholder).

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct TabDef {
    pub value: String,
    pub label: String,
    pub disabled: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTabs.poco")]
pub struct PineTabs {}

#[handlers]
impl PineTabs {}
