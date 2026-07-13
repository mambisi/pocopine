use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "owned-content-duplicate",
    template = "owned_content_duplicate.poco"
)]
struct OwnedContentDuplicate;

#[handlers]
impl OwnedContentDuplicate {}

fn main() {}
