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
mod resource;
#[cfg(not(target_arch = "wasm32"))]
mod source;

#[cfg(not(target_arch = "wasm32"))]
pub use async_trait::async_trait;
pub use id::{new_id, ResourceId};
pub use mutation::{CreatePayload, CrudMutationPayload, RemovePayload, SavePayload};
pub use options::{CreateOptions, RemoveOptions, SaveOptions, TransactionOptions, WritePolicy};
pub use outcome::{CrudOutcome, Queued, QueuedStatus};
#[cfg(not(target_arch = "wasm32"))]
pub use resource::{
    resource, CrudAcceptedMutation, CrudMutationLog, CrudResource, CrudResourceBuilder,
    MemoryCrudMutationLog, MissingMutationLog, NoRowVersion, RowVersionValue,
};
pub use row::optimistic_row;
#[cfg(not(target_arch = "wasm32"))]
pub use source::CrudSource;

#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub use resource::RowVersionOf;
pub use transaction::{Transaction, TransactionBindable, TransactionFuture, TransactionRunner};
