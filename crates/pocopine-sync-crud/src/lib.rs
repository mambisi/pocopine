//! Typed CRUD contracts for Pocopine sync.
//!
//! `pocopine-sync-crud` is an explicit extension crate. It provides the
//! resource identity, mutation payload, write-policy, and transaction-binding
//! contracts that a later proc macro will target. It does not generate SQL and
//! it is not an ORM.

mod id;
mod mutation;
mod options;
mod outcome;
mod row;
mod transaction;

#[cfg(not(target_arch = "wasm32"))]
mod source;

#[cfg(not(target_arch = "wasm32"))]
pub use async_trait::async_trait;
pub use id::{new_id, ResourceId};
pub use mutation::{CreatePayload, CrudMutationPayload, RemovePayload, SavePayload};
pub use options::{CreateOptions, RemoveOptions, SaveOptions, TransactionOptions, WritePolicy};
pub use outcome::{CrudOutcome, Queued, QueuedStatus};
pub use row::optimistic_row;
#[cfg(not(target_arch = "wasm32"))]
pub use source::CrudSource;
pub use transaction::{Transaction, TransactionBindable};
