//! Browser formatting and catalog delivery. Platform gating lives at lib.rs.
mod catalogs;
pub use catalogs::ClientCatalogs;
#[cfg(not(feature = "strict-parity"))]
pub(crate) mod intl;
