use pocopine::prelude::*;
struct State { a: u32, b: u32, c: u32 }
#[handlers]
impl State {
#[watch(a, writes(b))]
fn bad(a: FieldUpdate<u32>) -> bool { true }
}
fn main() {}
