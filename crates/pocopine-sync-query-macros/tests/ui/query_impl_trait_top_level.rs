//! `#[query]` must reject argument-position `impl Trait` (top-level).
use pocopine_sync_query::QueryClient;
use pocopine_sync_query_macros::query;
use std::fmt::Debug;

#[query]
fn bad_top_level(_client: QueryClient, x: impl Debug + Clone + std::hash::Hash + 'static) -> u32 {
    let _ = x;
    0
}

fn main() {}
