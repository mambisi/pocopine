//! RFC-097 §3.3 — an interior-mutability field type (`Cell`) must be
//! rejected: it would let a `&self` handler mutate state, breaking the
//! no-sweep optimisation.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(name = "fh-cell", template = poco! {<div></div>})]
struct CellComp {
    count: std::cell::Cell<u32>,
}

fn main() {}
