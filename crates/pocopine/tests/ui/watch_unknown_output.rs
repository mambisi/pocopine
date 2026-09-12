use pocopine::prelude::*;
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "test")]
struct State { value: u32 }
#[handlers]
impl State {
#[watch(value, updates(missing))]
fn bad(value: Change<u32>) -> Update<Self> { Update::new().missing(value.current) }
}
fn main() {}
