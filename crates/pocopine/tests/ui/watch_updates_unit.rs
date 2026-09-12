use pocopine::prelude::*;
struct State { value: u32, output: u32 }
#[handlers]
impl State {
#[watch(value, updates())]
fn empty(value: u32) { let _ = value; }
}
fn main() {}
