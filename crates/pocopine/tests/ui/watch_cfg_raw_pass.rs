#![deny(warnings)]
use pocopine::prelude::*;
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "raw")]
struct Raw { r#type: u32, r#match: u32, value: u32 }
#[handlers]
impl Raw {
    #[watch(r#type, writes(r#match))]
    pub fn forward(r#type: FieldUpdate<u32>) -> Update<Self> {
        Update::new().r#match(r#type.current)
    }
    #[cfg(any())]
    #[watch(r#match, writes(r#type))]
    fn inactive(r#match: FieldUpdate<u32>) -> Update<Self> {
        Update::new().r#type(r#match.current)
    }
    #[watch(value)]
    fn observe(value: FieldUpdate<u32>) { let _ = value; }
    #[watch(value)]
    fn empty(value: FieldUpdate<u32>) -> Update<Self> {
        let _ = value;
        Update::new()
    }
}
fn main() {}
