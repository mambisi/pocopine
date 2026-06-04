//! Landing section (`#secure`) — how auth works front-to-back across
//! the framework, and how the built-in observability contract makes
//! failures diagnosable and repairable. Copy + snippets are grounded
//! in the real primitives (`auth_plugin()`, `predicate_guard`,
//! `Server::with_auth`, `JwtVerifier`, `#[server(guard = …)]`, and the
//! RFC-069 `pocopine.*` event targets). The snippets are highlighted
//! by syntect at build time (see `crate::gen_code::secure`).

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::gen_code::secure;

#[derive(Default, Serialize, Deserialize)]
#[component(template = "SecureSection.poco", style = "secure_section.css")]
pub struct SecureSection {
    /// Pre-highlighted snippets (injected via pp-html).
    pub code_client: String,
    pub code_server: String,
    pub code_observe: String,
}

#[handlers]
impl SecureSection {
    pub fn on_setup(&mut self) {
        self.code_client = secure::code("client").into();
        self.code_server = secure::code("server").into();
        self.code_observe = secure::code("observe").into();
    }
}
