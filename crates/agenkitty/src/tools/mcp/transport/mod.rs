//! Injected MCP transports.
//!
//! agenkitty owns the transport rather than letting rmcp spawn children or open
//! sockets: [`stdio`] spawns the server through the process sandbox and hands
//! rmcp the child's `(stdout, stdin)`; [`http`] implements rmcp's
//! `StreamableHttpClient` over the network tool's SSRF-guarded / egress-proxied
//! client. Both land in their phases (stdio = Phase 1, http = Phase 4); Phase 0
//! only establishes the module layout.

pub mod http;
pub mod stdio;
