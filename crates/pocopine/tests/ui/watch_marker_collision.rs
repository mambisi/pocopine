use pocopine::prelude::*;
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "marker-collision")]
struct State { value: u32, _value: u32 }
fn main() {}
