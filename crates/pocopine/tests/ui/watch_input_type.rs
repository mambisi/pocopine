use pocopine::prelude::*;
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "test")]
struct State { value: u32 }
#[handlers]
impl State {
#[watch(value)]
fn bad(value: Change<String>) {}
}
fn main() {}
