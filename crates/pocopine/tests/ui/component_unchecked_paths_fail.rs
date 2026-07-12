// `unchecked_paths` compared against the literal string "true" — any
// other truthy spelling silently left path validation enabled.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "unchecked-paths-fixture",
    template_inline = "<div></div>",
    unchecked_paths = "yes"
)]
struct UncheckedPathsFixture {
    open: bool,
}

#[handlers]
impl UncheckedPathsFixture {}

fn main() {}
