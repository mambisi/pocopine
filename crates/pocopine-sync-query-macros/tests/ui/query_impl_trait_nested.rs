//! `#[query]` must reject argument-position `impl Trait` even when
//! nested inside another type — `Box<impl Hash>`, `Option<impl …>`,
//! etc. Rust still desugars these to anonymous generics, which would
//! let two concrete instantiations alias the same selector cache
//! entry.
use pocopine_sync_query::QueryClient;
use pocopine_sync_query_macros::query;
use std::hash::Hash;

#[query]
fn bad_nested(_client: QueryClient, b: Box<impl Hash + Clone + 'static>) -> u32 {
    let _ = b;
    0
}

fn main() {}
