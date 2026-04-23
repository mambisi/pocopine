use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "ContextMenuDemo.poco",
    style = "context_menu.css",
    role = "panel"
)]
pub struct ContextMenuDemo {}

#[handlers]
impl ContextMenuDemo {}
