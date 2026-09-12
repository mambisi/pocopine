use pocopine::prelude::*;
struct State { value: u32 }
#[handlers]
impl State {
    #[watch(updates(value))]
    fn bad(changes: Changes<Self>) -> Update<Self> { Update::new().value(1) }
}
fn main() {}
