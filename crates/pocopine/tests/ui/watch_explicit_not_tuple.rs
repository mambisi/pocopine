use pocopine::prelude::*;
struct State { value: u32, output: u32 }
#[handlers]
impl State {
#[watch(value)]
fn not_tuple(value: u32) -> Update<Self, (Self::Output)> { Update::new().output(value) }
}
fn main() {}
