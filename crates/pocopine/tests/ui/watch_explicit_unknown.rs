use pocopine::prelude::*;
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "invalid")]
struct State { value: u32, output: u32 }
#[handlers]
impl State {
#[watch(value)]
fn unknown(value: u32) -> Update<Self, (Self::Missing,)> { let _ = value; Update::new() }
}
fn main() {}
