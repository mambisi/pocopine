//! `PineDialog` — modal dialog primitive (placeholder).

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDialog.poco")]
pub struct PineDialog {}

#[handlers]
impl PineDialog {}
