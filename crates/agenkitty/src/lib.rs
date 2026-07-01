//! Agenkitty's host framework layer.
//!
//! Portable contracts live in `agenkitty-core`. This crate adds local runtime
//! wiring, project instruction discovery, event rendering, provider setup, and
//! a small CLI that can drive `pocopine-agenkit::server::AgentSession`.
//!
//! Orchestration and production host infrastructure are intentionally kept out
//! of this crate and provided by downstream hosts.
//!
//! `unsafe` is permitted but must be local, minimal, and carry a `// SAFETY:`
//! note (e.g. post-fork `pre_exec` resource limits in the process tool). A
//! capable host harness cannot enforce kernel-level sandboxing without it.

pub use agenkitty_core::*;

#[cfg(not(target_arch = "wasm32"))]
pub mod args;
#[cfg(not(target_arch = "wasm32"))]
pub mod config;
#[cfg(not(target_arch = "wasm32"))]
pub mod events;
#[cfg(not(target_arch = "wasm32"))]
pub mod memory;
#[cfg(not(target_arch = "wasm32"))]
pub mod policy;
#[cfg(not(target_arch = "wasm32"))]
pub mod project;
#[cfg(not(target_arch = "wasm32"))]
pub mod sandbox_api;
#[cfg(not(target_arch = "wasm32"))]
pub mod sessions;
#[cfg(not(target_arch = "wasm32"))]
pub mod supervisor;
#[cfg(not(target_arch = "wasm32"))]
pub mod tools;

#[cfg(not(target_arch = "wasm32"))]
pub use supervisor::{AgentRunOptions, AgentRunReport, FrameworkRunner, QwenProviderConfig};
