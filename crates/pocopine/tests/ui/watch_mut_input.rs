use pocopine::prelude::*;
struct State { value: u32 }
#[handlers]
impl State {
    #[watch(value)]
    fn bad(value: &mut u32) { *value = 2; }
}
fn main() {}
