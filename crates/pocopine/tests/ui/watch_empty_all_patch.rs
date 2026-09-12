use pocopine::prelude::*;
struct State { value: u32, output: u32 }
#[handlers]
impl State {
#[watch]
fn empty(changes: Changes<Self>) -> Update<Self, ()> { let _ = changes; Update::new() }
}
fn main() {}
