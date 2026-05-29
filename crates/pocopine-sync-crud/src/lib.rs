//! Typed CRUD contracts for Pocopine sync.
//!
//! `pocopine-sync-crud` is an explicit extension crate. It provides the
//! resource identity, mutation payload, write-policy, and transaction-binding
//! contracts that a later proc macro will target. It does not generate SQL and
//! it is not an ORM.

pub use pocopine_sync_crud_macros::resource;

mod client;
mod id;
mod mutation;
mod options;
mod outcome;
mod row;
mod subscription;
mod transaction;
mod view;

#[cfg(not(target_arch = "wasm32"))]
mod resource;
#[cfg(not(target_arch = "wasm32"))]
mod source;

#[cfg(not(target_arch = "wasm32"))]
pub use async_trait::async_trait;
pub use client::{client_resource, CrudClientResource};
pub use id::{new_id, ResourceId};
pub use mutation::{CreatePayload, CrudMutationPayload, RemovePayload, SavePayload};
pub use options::{CreateOptions, RemoveOptions, SaveOptions, TransactionOptions, WritePolicy};
pub use outcome::{CrudOutcome, Queued, QueuedStatus};
#[cfg(not(target_arch = "wasm32"))]
pub use resource::{
    resource, CrudAcceptedMutation, CrudMigrateFn, CrudMutationLog, CrudMutationReservation,
    CrudResource, CrudResourceBuilder, MemoryCrudMutationLog, MemoryCrudScopeFn,
    MissingMutationLog, NoRowVersion, RowVersionValue, TransactionalCrudMutationLog,
    TransactionalCrudResource, DEFAULT_CRUD_SNAPSHOT_ROW_LIMIT,
};
// RFC 090 Phase 2a — `Crud*` lifecycle types (Conflict, WriteResult,
// RemoveResult, MutationLog, AcceptedMutation, etc.) are now type
// aliases for the canonical types in `pocopine_sync_query::write`.
// External code that imports `pocopine_sync_crud::Crud*` keeps
// working byte-for-byte. The re-exports in `resource::*` and
// `source::*` above pick up the aliases automatically — no change
// needed here. This comment marks the migration boundary for Phase
// 5 deprecation notes and Phase 6 deletion.
pub use row::optimistic_row;
#[cfg(not(target_arch = "wasm32"))]
pub use source::{
    CrudConflict, CrudRemoveResult, CrudSource, CrudWriteResult, TransactionalCrudSource,
};
pub use subscription::observe_local_resource_view;

#[cfg(not(target_arch = "wasm32"))]
pub use resource::RowVersionOf;
pub use transaction::{
    CrudTransactionRunner, Transaction, TransactionBindable, TransactionFuture, TransactionRunner,
};
pub use view::{
    local_resource_view, LocalResourcePendingMutation, LocalResourceRow, LocalResourceRowStatus,
    LocalResourceView, LocalResourceViewState,
};
