//! Landing section (`#full-stack`) — a diagram of how pocopine's
//! framework components work together end to end. Copy is grounded in
//! the real primitives (`#[component]`, `#[server]`,
//! `#[query_resource]`, `Source`, auth, jobs, deploy, observability).
//! Stateless; composition lives in `StackFlow.poco`.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "StackFlow.poco", style = "stack_flow.css")]
pub struct StackFlow {}

#[handlers]
impl StackFlow {}
