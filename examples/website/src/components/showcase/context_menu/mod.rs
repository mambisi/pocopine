use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "ContextMenuDemo.poco", role = "panel")]
pub struct ContextMenuDemo {}

#[handlers]
impl ContextMenuDemo {}
