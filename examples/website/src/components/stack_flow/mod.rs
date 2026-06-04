//! Landing section (`#full-stack`) — "One language, the whole flow".
//! Follows one request through the stack as a scroll-spy walkthrough: a
//! column of steps (each with a real, build-time-highlighted snippet)
//! drives a sticky, interactive issue-tracker preview (`<issue-flow-demo
//! :stage>`) that builds up, goes live, and ships as you scroll. Copy is
//! grounded in the real primitives (`#[component]`, `#[server]`,
//! `#[query_resource]`, `Source`, auth, jobs, deploy, observability).

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::gen_code::flow;

#[derive(Default, Serialize, Deserialize)]
#[component(template = "StackFlow.poco", style = "stack_flow.css")]
pub struct StackFlow {
    /// Active step (1-based) — driven by scroll via pp-intersect; bound
    /// to the demo's `:stage` and the step-number highlight.
    pub active: u32,
    /// Per-step snippets, pre-highlighted by syntect (see build.rs).
    pub code1: String,
    pub code2: String,
    pub code3: String,
    pub code4: String,
    pub code5: String,
}

#[handlers]
impl StackFlow {
    pub fn on_setup(&mut self) {
        self.active = 1;
        self.code1 = flow::code(0).into();
        self.code2 = flow::code(1).into();
        self.code3 = flow::code(2).into();
        self.code4 = flow::code(3).into();
        self.code5 = flow::code(4).into();
    }

    pub fn enter1(&mut self) {
        self.active = 1;
    }
    pub fn enter2(&mut self) {
        self.active = 2;
    }
    pub fn enter3(&mut self) {
        self.active = 3;
    }
    pub fn enter4(&mut self) {
        self.active = 4;
    }
    pub fn enter5(&mut self) {
        self.active = 5;
    }
}
