use pocopine::prelude::*;
struct State { value: u32 }
#[handlers]
impl State {
    #[watch]
    fn bad(value: &u32) {}
}
fn main() {}
