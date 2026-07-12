// Duplicate #[component(...)] keys used to last-one-win silently —
// the earlier value vanished with no diagnostic.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "dup-key-fixture",
    template_inline = "<div></div>",
    role = "panel",
    role = "toolbar"
)]
struct DupKeyFixture {
    open: bool,
}

#[handlers]
impl DupKeyFixture {}

fn main() {}
