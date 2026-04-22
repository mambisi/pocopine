//! Four-step install tutorial. Stateless; the numbered cards +
//! terminal-styled code blocks live entirely in the template.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "Tutorial.poco", role = "panel")]
pub struct Tutorial {}

#[handlers]
impl Tutorial {}
