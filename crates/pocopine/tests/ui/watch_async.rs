use pocopine::prelude::*;
struct State { a: u32, b: u32, c: u32 }
#[handlers]
impl State {
#[watch(a)]
async fn bad(a: Change<u32>) {}
}
fn main() {}
