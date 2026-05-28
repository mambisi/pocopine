//! `#[query]` must reject `impl Trait` embedded in trait-object
//! assoc-type bindings — e.g. `*const dyn Foo<Item = impl Bar>`.
//! The argument is still anonymous-generic at the call site.
use pocopine_sync_query::QueryClient;
use pocopine_sync_query_macros::query;
use std::fmt::Debug;

trait Assoc {
    type Item;
}

#[query]
fn bad_assoc_impl(
    _client: QueryClient,
    x: *const dyn Assoc<Item = impl Debug + 'static>,
) -> u32 {
    let _ = x;
    0
}

fn main() {}
