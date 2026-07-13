use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "owned-content-structural",
    template = "owned_content_structural.poco"
)]
struct OwnedContentStructural {
    open: bool,
}

#[handlers]
impl OwnedContentStructural {}

fn main() {}
