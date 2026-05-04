//! In-tree provider configs.
//!
//! Each provider is a typed config struct implementing
//! [`crate::Provider`]. New providers ship in their own follow-up
//! PR with a recorded-token integration test (RFC-074 §5.3.1) —
//! no aspirational presets that haven't been validated against
//! real provider tokens.
//!
//! Third-party providers belong in their own crate following the
//! `pocopine-auth-jwt-<vendor>` naming convention, not in this
//! module. See `docs/auth-jwt-providers.md` for the contract and
//! the community-maintained list.

#![cfg(not(target_arch = "wasm32"))]

pub mod firebase;

pub use firebase::Firebase;
