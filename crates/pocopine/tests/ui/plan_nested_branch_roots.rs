use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "plan-nested-branch-roots",
    template = "plan_nested_branch_roots.poco"
)]
struct PlanNestedBranchRoots {
    open: bool,
    state: String,
}

#[handlers]
impl PlanNestedBranchRoots {}

fn main() {}
