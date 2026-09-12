use pocopine::prelude::*;
struct State { value: u32, output: u32 }
#[handlers]
impl State {
#[watch(value)]
fn overlap(value: u32) -> Update<Self, (Self::Value,)> { Update::new().value(value) }
}
fn main() {}
