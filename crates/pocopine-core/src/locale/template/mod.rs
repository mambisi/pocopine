use crate::StaticExpr;
use pocopine_locale::CompiledMessage;

#[derive(Clone, Copy, Debug)]
pub struct TranslationPlan {
    pub message: CompiledMessage,
    pub arguments: &'static [&'static StaticExpr],
}

#[cfg(target_arch = "wasm32")]
mod client;
#[cfg(target_arch = "wasm32")]
pub use client::{install, value};
#[cfg(not(target_arch = "wasm32"))]
mod server;
#[cfg(not(target_arch = "wasm32"))]
pub use server::{install, value};
