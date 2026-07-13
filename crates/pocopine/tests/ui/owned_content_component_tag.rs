use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "owned-content-component-tag",
    template_inline = "<main><external-panel pp-owned-content></external-panel></main>"
)]
struct OwnedContentComponentTag;

#[handlers]
impl OwnedContentComponentTag {}

fn main() {}
