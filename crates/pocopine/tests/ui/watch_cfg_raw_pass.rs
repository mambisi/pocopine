#![deny(warnings)]
use pocopine::prelude::*;
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "raw")]
struct Raw { r#type: u32, r#match: u32, value: u32, _1: u32, self_: u32 }
#[handlers]
impl Raw {
    #[watch(r#type, updates(r#match))]
    pub fn forward(r#type: Change<u32>) -> Update<Self> {
        Update::new().r#match(r#type.current)
    }
    #[cfg(any())]
    #[watch(r#match, updates(r#type))]
    fn inactive(r#match: Change<u32>) -> Update<Self> {
        Update::new().r#type(r#match.current)
    }
    #[watch(value)]
    fn unusual_names(value: u32) -> Update<Self, (Self::_1, Self::Self_)> {
        Update::new()._1(value).self_(value)
    }
    #[watch(value)]
    fn observe(value: Change<u32>) { let _ = value; }
    #[watch(value, updates(r#match))]
    fn empty(value: Change<u32>) -> Update<Self> {
        let _ = value;
        Update::new()
    }
}
fn main() {}
