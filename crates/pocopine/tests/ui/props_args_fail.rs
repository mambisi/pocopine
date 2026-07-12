// `#[prop(...)]` arguments in a #[derive(Props)] leaf struct used to
// be silently discarded — `name`/`skip`-style intent evaporated.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Props, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesCommon {
    #[prop(name = "series-key")]
    pub key: String,
    #[prop]
    pub label: String,
}

fn main() {}
