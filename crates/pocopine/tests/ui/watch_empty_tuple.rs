use pocopine::prelude::*;
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "invalid")]
struct State { value: u32, output: u32 }
#[handlers]
impl State {
#[deny(deprecated)]
#[watch(value)]
fn empty(value: u32) -> Update<Self, ()> { let _ = value; Update::new() }
}
fn main() {}
