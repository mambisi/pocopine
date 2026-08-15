use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

struct NotAContextExtractor;

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "handler-context-ui-missing-trait",
    template = poco! { <button>fail</button> }
)]
struct MissingTrait {}

#[handlers]
impl MissingTrait {
    fn broken(&mut self, #[context] _value: NotAContextExtractor) {}
}

fn main() {}
