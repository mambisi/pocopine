use pocopine::prelude::*;
struct State { value: u32 }
#[handlers]
impl State {
    #[watch]
    fn bad(changes: Changes<Self>) -> Update<Self> { Update::new() }
}
fn main() {}
