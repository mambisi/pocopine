use pocopine::prelude::*;
mod model {
    use super::*;
    #[derive(Default, serde::Serialize, serde::Deserialize)]
    #[store(name = "marker-visibility")]
    pub struct State { private_value: u32 }
    #[handlers]
    impl State {}
}
fn main() { let _ = model::StateField::PrivateValue::get(&model::State::default()); }
