use pocopine::prelude::*;
struct State { a: u32, b: u32, c: u32 }
#[handlers]
impl State {
#[watch(a, writes(a))]
fn bad(a: Change<u32>) -> Update<Self> { Update::new().a(a.current) }
}
fn main() {}
