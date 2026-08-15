use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "handler-context-ui-pass",
    template = poco! { <button>pass</button> }
)]
struct ContextPositions {}

#[handlers]
impl ContextPositions {
    fn before(&mut self, #[context] _context: HandlerContext, _value: String) {}

    fn after(&mut self, _value: String, #[context] _scope: ScopeId) {}
}

fn main() {}
