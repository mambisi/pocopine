use pocopine::prelude::*;
struct State { value: u32, output: u32 }
#[handlers]
impl State {
#[watch(value)]
fn missing(value: u32) -> Update<Self> { let _ = value; Update::new() }
}
fn main() {}
