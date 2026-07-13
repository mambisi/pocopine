use pocopine_collab::CompatibilityIdentity;

pub const PROTOCOL_VERSION: u16 = 1;
pub const FINGERPRINT: &str = "24e7376d7f202530e56aeeaff79e191746a8e08a8d6af44b184443058259f9f1";
#[cfg(target_arch = "wasm32")]
pub const DOCUMENT_KEY: &str = "canvas-demo";

pub fn identity() -> CompatibilityIdentity {
    CompatibilityIdentity::new(PROTOCOL_VERSION, FINGERPRINT)
        .expect("collab-canvas compatibility fingerprint is canonical")
}

#[cfg(target_arch = "wasm32")]
pub fn topic() -> String {
    identity().namespace_topic(DOCUMENT_KEY)
}
