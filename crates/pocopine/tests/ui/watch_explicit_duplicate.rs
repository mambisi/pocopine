use pocopine::prelude::*;
struct State { value: u32, output: u32 }
#[handlers]
impl State {
#[watch(value)]
fn duplicate(value: u32) -> Update<Self, (Self::Output, Self::Output)> { let _ = value; Update::new() }
}
fn main() {}
