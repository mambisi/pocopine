//! Heroku-style deploy contract for pocopine apps (RFC 080).
//!
//! Public surface:
//!
//! - [`DeploySpec`] — parsed `[deploy]` block from `Pocopine.toml`
//!   plus computed values (inferred service requirements, build flags).
//! - [`DeployAdapter`] — the trait every vendor crate implements (§5.2).
//! - [`common`] — shared helpers (Dockerfile generation, `.dockerignore`).
//!
//! Vendor adapters live in their own crates (`pocopine-deploy-railway`,
//! `pocopine-deploy-render`, …) and depend on this one. Adapters talk
//! to each host's API directly; no host CLIs are shelled out (RFC 080
//! §2.3, §11 q7). Only `docker` is an acceptable external binary in
//! the deploy path.

#![forbid(unsafe_code)]

pub mod adapter;
pub mod artefact;
pub mod common;
pub mod config;
pub mod credentials;
pub mod docker;
pub mod registry;
pub mod spec;

pub use adapter::{
    AdapterMode, AdapterSource, Constraint, DeployAdapter, DeployOutcome, DeployState, Hint,
    ProcessStatus,
};
pub use artefact::{Artefact, StagedFiles};
pub use registry::{
    resolve_registry, resolve_registry_credentials, GitProvider, RegistryCredentials,
    ResolvedRegistry,
};
pub use spec::{DeploySpec, EnvSource, EnvValue, Mode, ProcessSpec, Scale, ServiceSpec};
