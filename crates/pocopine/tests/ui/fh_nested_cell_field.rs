//! RFC-097 §3.3 — interior mutability nested under a wrapper must also
//! be rejected. `Option<Cell<u32>>` serializes fine (so the serde bound
//! doesn't catch it) yet still lets a `&self` handler mutate the cell,
//! so the recursive type walk must reject it. A preceding tuple field
//! also guards against the early-return regression (a non-path field
//! must not abort the scan).
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(name = "fh-nested-cell", template = poco! {<div></div>})]
struct NestedCellComp {
    coords: (f64, f64),
    guard: Option<std::cell::Cell<u32>>,
}

fn main() {}
