//! The tool-definition pin store (D8/T2): trust-on-first-use plus rug-pull
//! detection over each tool's [`pin_hash`](super::common::pin_hash).
//!
//! When a server is discovered, every tool is **observed** into the store: the
//! first sighting records its hash (TOFU). A host then **approves** a specific
//! hash. If the server later serves a *different* description or schema for the
//! same id (a rug pull — the "sleeper" server), the new hash no longer matches
//! the approved one, approval is cleared, and the tool is disabled pending
//! re-approval. A silent `tools/list_changed` swap is therefore never adopted:
//! the swapped definition fails the [`is_approved`](PinStore::is_approved)
//! check.
//!
//! The store is in-memory and session-scoped for v1 (project persistence is
//! deferred, OQ4); it lives on the [`McpRuntime`](super::runtime::McpRuntime)
//! and is shared across its clones.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// The result of [`PinStore::observe`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinStatus {
    /// First time this `(server, tool)` was seen; the hash was recorded.
    New,
    /// The hash matches the recorded pin (no change since last sighting).
    Unchanged,
    /// The hash differs from the recorded pin — a rug pull. Any prior approval
    /// is cleared; the tool is disabled pending re-approval (T2).
    Changed,
}

struct PinRecord {
    /// The most recently observed `pin_hash`.
    hash: String,
    /// Whether the *current* `hash` has been approved by the host.
    approved: bool,
}

/// Session-scoped store of observed/approved tool-definition pins, keyed by
/// `(server, tool)`.
#[derive(Default)]
pub struct PinStore {
    records: Mutex<HashMap<(String, String), PinRecord>>,
}

impl PinStore {
    /// Record a sighting of `(server, tool)` at `hash` (TOFU). Returns whether
    /// this is a new pin, an unchanged re-sighting, or a *changed* definition
    /// (rug pull) — in which case the prior approval is cleared.
    pub fn observe(&self, server: &str, tool: &str, hash: &str) -> PinStatus {
        let mut records = self.records.lock().expect("mcp pin store poisoned");
        let key = (server.to_string(), tool.to_string());
        match records.get_mut(&key) {
            None => {
                records.insert(
                    key,
                    PinRecord {
                        hash: hash.to_string(),
                        approved: false,
                    },
                );
                PinStatus::New
            }
            Some(record) if record.hash == hash => PinStatus::Unchanged,
            Some(record) => {
                // Rug pull: the definition changed. Re-pin and revoke approval.
                record.hash = hash.to_string();
                record.approved = false;
                PinStatus::Changed
            }
        }
    }

    /// Approve the pin for `(server, tool)` at `hash`. Records the hash if it was
    /// never observed (an allowlisted tool can be approved on first sight). The
    /// approval is bound to this exact hash: a later [`observe`](Self::observe)
    /// with a different hash clears it.
    pub fn approve(&self, server: &str, tool: &str, hash: &str) {
        let mut records = self.records.lock().expect("mcp pin store poisoned");
        records.insert(
            (server.to_string(), tool.to_string()),
            PinRecord {
                hash: hash.to_string(),
                approved: true,
            },
        );
    }

    /// Whether `(server, tool)` is approved **and** its approved hash matches the
    /// supplied `hash`. A rug-pulled tool (changed hash) returns `false` until it
    /// is re-approved.
    pub fn is_approved(&self, server: &str, tool: &str, hash: &str) -> bool {
        let records = self.records.lock().expect("mcp pin store poisoned");
        records
            .get(&(server.to_string(), tool.to_string()))
            .is_some_and(|record| record.approved && record.hash == hash)
    }

    /// Whether the **currently observed** hash for `(server, tool)` equals
    /// `hash` — regardless of approval. The interactive approval flow uses this
    /// to refuse to even *prompt* for a definition that is no longer the live
    /// one (the operator would be judging stale metadata).
    pub fn is_current(&self, server: &str, tool: &str, hash: &str) -> bool {
        let records = self.records.lock().expect("mcp pin store poisoned");
        records
            .get(&(server.to_string(), tool.to_string()))
            .is_some_and(|record| record.hash == hash)
    }

    /// Approve the pin for `(server, tool)` **only if** its currently observed
    /// hash equals `hash`; returns whether the approval was recorded. Unlike
    /// [`approve`](Self::approve) this never overwrites a newer observation, so
    /// an interactive approval races safely with a concurrent rediscovery: a
    /// definition that changed after the prompt was shown stays unapproved.
    pub fn approve_if_current(&self, server: &str, tool: &str, hash: &str) -> bool {
        let mut records = self.records.lock().expect("mcp pin store poisoned");
        match records.get_mut(&(server.to_string(), tool.to_string())) {
            Some(record) if record.hash == hash => {
                record.approved = true;
                true
            }
            _ => false,
        }
    }

    /// Drop the pin record of every tool of `server` whose name is **not** in
    /// `present` (T2): a tool that disappeared on a `tools/list_changed`
    /// re-discovery can no longer be dispatched under its now-stale approval —
    /// and its stale record must not satisfy [`is_current`](Self::is_current)
    /// either, or the interactive approval flow could bind an approval to a
    /// tool the server no longer serves. A tool that later reappears is a
    /// fresh TOFU sighting. Returns the tool names whose **approval** was
    /// revoked (never-approved records are dropped silently).
    pub fn revoke_missing(&self, server: &str, present: &HashSet<String>) -> Vec<String> {
        let mut records = self.records.lock().expect("mcp pin store poisoned");
        let mut cleared = Vec::new();
        records.retain(|(s, t), record| {
            if s == server && !present.contains(t) {
                if record.approved {
                    cleared.push(t.clone());
                }
                false
            } else {
                true
            }
        });
        cleared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_observation_is_new_then_unchanged() {
        let store = PinStore::default();
        assert_eq!(store.observe("s", "t", "hash-a"), PinStatus::New);
        assert_eq!(store.observe("s", "t", "hash-a"), PinStatus::Unchanged);
        // Not approved until a host approves it.
        assert!(!store.is_approved("s", "t", "hash-a"));
    }

    #[test]
    fn approval_is_bound_to_the_hash() {
        let store = PinStore::default();
        store.observe("s", "t", "hash-a");
        store.approve("s", "t", "hash-a");
        assert!(store.is_approved("s", "t", "hash-a"));
        // A different hash is never considered approved.
        assert!(!store.is_approved("s", "t", "hash-b"));
    }

    #[test]
    fn rug_pull_changes_status_and_clears_approval() {
        let store = PinStore::default();
        store.observe("s", "t", "hash-a");
        store.approve("s", "t", "hash-a");
        assert!(store.is_approved("s", "t", "hash-a"));

        // The server swaps the definition (e.g. via tools/list_changed): the new
        // hash is a Changed observation and approval is revoked.
        assert_eq!(store.observe("s", "t", "hash-b"), PinStatus::Changed);
        assert!(!store.is_approved("s", "t", "hash-a"));
        assert!(!store.is_approved("s", "t", "hash-b"));

        // Re-approval of the new definition restores access for that hash only.
        store.approve("s", "t", "hash-b");
        assert!(store.is_approved("s", "t", "hash-b"));
        assert!(!store.is_approved("s", "t", "hash-a"));
    }

    #[test]
    fn approve_without_prior_observation_records_the_pin() {
        let store = PinStore::default();
        store.approve("s", "t", "hash-a");
        assert!(store.is_approved("s", "t", "hash-a"));
    }

    #[test]
    fn approve_if_current_binds_to_the_live_observation() {
        let store = PinStore::default();
        // Never observed → nothing to bind to; refused (unlike `approve`, which
        // records unseen pins for the operator-allowlist path).
        assert!(!store.approve_if_current("s", "t", "hash-a"));
        assert!(!store.is_approved("s", "t", "hash-a"));

        store.observe("s", "t", "hash-a");
        assert!(store.is_current("s", "t", "hash-a"));
        assert!(store.approve_if_current("s", "t", "hash-a"));
        assert!(store.is_approved("s", "t", "hash-a"));

        // Rug pull: the stale hash is no longer current and can NOT be
        // re-approved — the interactive flow must fail closed, never resurrect
        // a definition the operator didn't actually see.
        assert_eq!(store.observe("s", "t", "hash-b"), PinStatus::Changed);
        assert!(!store.is_current("s", "t", "hash-a"));
        assert!(!store.approve_if_current("s", "t", "hash-a"));
        assert!(!store.is_approved("s", "t", "hash-a"));
        assert!(!store.is_approved("s", "t", "hash-b"));
    }

    #[test]
    fn revoke_missing_clears_only_disappeared_tools() {
        let store = PinStore::default();
        store.approve("s", "kept", "hash-kept");
        store.approve("s", "gone", "hash-gone");
        store.approve("other", "gone", "hash-other"); // a different server, untouched

        let present: HashSet<String> = ["kept".to_string()].into_iter().collect();
        let cleared = store.revoke_missing("s", &present);

        assert_eq!(cleared, vec!["gone".to_string()]);
        assert!(store.is_approved("s", "kept", "hash-kept"));
        assert!(!store.is_approved("s", "gone", "hash-gone"));
        // A same-named tool on another server is not affected.
        assert!(store.is_approved("other", "gone", "hash-other"));
    }

    #[test]
    fn removed_tools_drop_their_record_and_cannot_be_approved() {
        let store = PinStore::default();
        // One approved, one merely observed (never approved) — both vanish
        // from the server's live list.
        store.approve("s", "gone-approved", "hash-a");
        store.observe("s", "gone-observed", "hash-b");

        let present: HashSet<String> = HashSet::new();
        let cleared = store.revoke_missing("s", &present);
        assert_eq!(cleared, vec!["gone-approved".to_string()]);

        // Neither stale record satisfies `is_current` — the interactive
        // approval flow must never prompt for (or bind an approval to) a tool
        // the server no longer serves.
        assert!(!store.is_current("s", "gone-approved", "hash-a"));
        assert!(!store.is_current("s", "gone-observed", "hash-b"));
        assert!(!store.approve_if_current("s", "gone-approved", "hash-a"));
        assert!(!store.approve_if_current("s", "gone-observed", "hash-b"));
        assert!(!store.is_approved("s", "gone-approved", "hash-a"));
        assert!(!store.is_approved("s", "gone-observed", "hash-b"));

        // A reappearing tool is a fresh TOFU sighting, unapproved.
        assert_eq!(
            store.observe("s", "gone-approved", "hash-a"),
            PinStatus::New
        );
        assert!(!store.is_approved("s", "gone-approved", "hash-a"));
    }
}
