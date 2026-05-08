//! Default user-id generator — millis-prefix + UUIDv7.
//!
//! Produces values shaped like `"01a8b8c2d4e6-018e6e8f-7c2d-7b54-..."`
//! — sortable by creation time and collision-resistant. Apps that
//! want their own scheme (snowflake, ULID, sequential) override via
//! `Credentials::with_id_generator`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Build a fresh user id. Allocates one short String per call.
pub fn default_id_generator() -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let uuid = uuid::Uuid::now_v7();
    format!("{now_ms:x}-{uuid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_lexicographically_sortable() {
        let a = default_id_generator();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = default_id_generator();
        assert!(a < b, "expected later id `{b}` to sort after earlier `{a}`");
    }

    #[test]
    fn ids_are_unique_under_burst() {
        let ids: std::collections::HashSet<_> =
            (0..1000).map(|_| default_id_generator()).collect();
        assert_eq!(ids.len(), 1000);
    }
}
