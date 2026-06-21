use super::{MAX_SYNC_TOKEN_LEN, StreamParams, SyncStreamName};

/// Live query tag used to wake clients for a sync stream.
pub fn sync_stream_tag(stream: &str) -> String {
    format!("sync:stream:{stream}")
}

/// Number of lowercase-hex digits in the per-params topic suffix
/// (RFC 088 §C). The suffix is `{params_hash:016x}` — a `u64`
/// formatted as 16 lowercase hex digits — so the validator in
/// `pocopine_live::is_params_hash_suffix` checks against
/// `PARAMS_HASH_HEX_LEN + 1` total bytes (the leading `:` plus 16
/// hex digits). Exporting the constant keeps the formatter and the
/// validator coupled so a future widening of the hash (say, to 128
/// bits / 32 hex chars) updates both call sites simultaneously.
pub const PARAMS_HASH_HEX_LEN: usize = 16;

/// Per-(stream, params) live query tag, used for RFC 088 §C precise
/// invalidation routing. Same prefix as [`sync_stream_tag`] plus a
/// `PARAMS_HASH_HEX_LEN`-hex suffix derived from
/// [`stream_params_hash`].
///
/// Wire shape: `sync:stream:{stream}:{params_hash:016x}`.
///
/// Mirrors the bare-tag convention; clients on the new protocol
/// subscribe to BOTH the bare and per-params topics during rollout
/// (see RFC 088 §C.6). Servers publish to per-params iff the source
/// returns non-empty params from
/// [`SyncStreamSource::row_to_params`](crate::server::SyncStreamSource::row_to_params).
pub fn sync_stream_params_tag(stream: &str, params_hash: u64) -> String {
    format!("sync:stream:{stream}:{params_hash:016x}")
}

/// FNV-1a 64-bit hash over `(stream_name, sorted-by-key params JSON)`.
/// Closely related to [`local_stream_key`] — both use the same
/// FNV-1a constants and the same `stream + 0x00 + canonical_json`
/// byte recipe — but with one edge-case difference: `local_stream_key`
/// short-circuits to the bare stream name for `params.is_empty()`,
/// while this function always hashes (folding in the separator byte
/// plus the empty-object JSON `"{}"`). In practice the wire-side
/// path only calls this for non-empty params (the publish helper
/// guards on empty), so the divergence is invisible; cross-language
/// porters matching this byte spec for cache scoping should mirror
/// the always-hash form here rather than copying
/// `local_stream_key`'s short-circuit.
///
/// This is the cross-language contract for RFC 088 §C — the
/// SERVER computes this from the row's `row_to_params` projection,
/// the CLIENT computes it from the captured subscription params,
/// and the two MUST agree byte-for-byte. The hash function is FNV-1a
/// (not cryptographic) and `StreamParams = BTreeMap<String, Value>`
/// serializes in sorted-key order by construction, so the only
/// portability constraints are: same FNV-1a constants, same JSON
/// encoding (no insignificant whitespace, no number/float coercion).
///
/// The golden-fixture test at
/// `tests/stream_params_hash_fixture.rs` pins a set of (stream,
/// params) → hex-hash pairs that future ports (Swift / Go / Kotlin
/// clients) MUST reproduce.
pub fn stream_params_hash(stream: &str, params: &StreamParams) -> u64 {
    // Canonical JSON of a `BTreeMap<String, Value>` is infallible
    // for any value that `serde_json::Value` can hold (Value rejects
    // NaN/Infinity at construction time). The only documented
    // failure path is `serde_json`'s default 128-level recursion
    // limit on deeply-nested `Value::Object`/`Value::Array`. A
    // silent `unwrap_or_default()` here would collapse every such
    // failure to the same hash (FNV-1a of `stream + 0x00`), aliasing
    // distinct subscriptions onto one topic — codex caught the
    // regression in review. We `expect()` instead so the failure
    // surfaces as a clear panic at the publish site rather than as
    // mis-routed live wakeups in production.
    let canonical = serde_json::to_vec(params)
        .expect("BTreeMap<String, Value> canonical JSON is infallible for serde_json::Value");
    let mut buf: Vec<u8> = Vec::with_capacity(stream.len() + 1 + canonical.len());
    buf.extend_from_slice(stream.as_bytes());
    // Separator so `stream="ab", params={}` ≠ `stream="a", params={b:…}`.
    buf.push(0u8);
    buf.extend_from_slice(&canonical);
    fnv1a_64(&buf)
}

/// Stable client-side cache key that combines a sync stream name with
/// a hash of its subscription params. Used to scope the local store so
/// two subscriptions to the same `stream` with different `params` do
/// NOT share cached rows, cursor, or pending mutations.
///
/// **Wire-vs-local split:** every `/open`, `/pull`, and `/push`
/// envelope carries the BARE `stream` plus a `params` map. The local
/// store, by contrast, sees a composite name so its existing
/// stream-keyed API works without a breaking change.
///
/// **Backwards-compat:** when `params` is empty, the returned key is
/// the bare stream name (identical to pre-RFC-085 on-disk data). Old
/// snapshots in any `SyncLocalStore` impl keep hydrating cleanly for
/// unparameterized subscriptions.
///
/// **Hash:** FNV-1a 64-bit over the canonical JSON encoding of the
/// `BTreeMap<String, Value>` (sorted by key by construction). This is
/// NOT a cryptographic hash — it's purely for cache disambiguation
/// inside one client; collisions would shred a single client's cache
/// but never leak data cross-client.
pub fn local_stream_key(stream: &SyncStreamName, params: &StreamParams) -> SyncStreamName {
    if params.is_empty() {
        return stream.clone();
    }
    let stream_str = stream.as_str();
    // Hash the FULL stream name plus a separator plus the canonical
    // params JSON. Two long stream names that share a truncated
    // prefix but differ in their suffix would otherwise produce
    // identical composite keys after the prefix truncation below.
    // Mixing the full stream into the hash gives every (stream, params)
    // pair a unique 64-bit suffix even if the visible prefix collides.
    let canonical = serde_json::to_vec(params).unwrap_or_default();
    let mut hasher_input: Vec<u8> = Vec::with_capacity(stream_str.len() + 1 + canonical.len());
    hasher_input.extend_from_slice(stream_str.as_bytes());
    hasher_input.push(0u8); // separator so `stream_str=ab + p={}` ≠ `stream_str=a + p=b{}`.
    hasher_input.extend_from_slice(&canonical);
    let hash = fnv1a_64(&hasher_input);
    let suffix = format!("__params_{:016x}", hash);
    // Cap the composite at MAX_SYNC_TOKEN_LEN by truncating the stream
    // prefix at a char boundary. Even when the prefix is truncated, the
    // suffix carries the full-stream + params hash, so distinct streams
    // with the same params still get distinct composite keys.
    let max_stream_len = MAX_SYNC_TOKEN_LEN.saturating_sub(suffix.len());
    let stream_part = if stream_str.len() <= max_stream_len {
        stream_str
    } else {
        let mut end = max_stream_len;
        while end > 0 && !stream_str.is_char_boundary(end) {
            end -= 1;
        }
        &stream_str[..end]
    };
    let composite = format!("{stream_part}{suffix}");
    // Truncation preserves byte composition up to a char boundary;
    // the source `stream_str` already passed `validate_token`, so the
    // truncated prefix + ASCII suffix still satisfies the token
    // contract. Any failure here would indicate a framework bug.
    SyncStreamName::new(composite).expect("local_stream_key fits the sync token budget")
}

/// FNV-1a 64-bit. Inline because we don't want a hash-crate dep for
/// the four bytes of work this does.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    #[test]
    fn local_stream_key_is_bare_for_empty_params() {
        // Backwards-compat: pre-RFC-085 cache entries are keyed by
        // bare stream name. An empty params map must produce the same
        // key so existing on-disk snapshots keep hydrating.
        let stream = SyncStreamName::new("posts").unwrap();
        let key = local_stream_key(&stream, &BTreeMap::new());
        assert_eq!(key.as_str(), "posts");
    }

    #[test]
    fn local_stream_key_differs_per_distinct_params() {
        // Cache disambiguation: two subscriptions to the same stream
        // with different params produce different keys, so they don't
        // overwrite each other's cursor/rows/queue in the local store.
        let stream = SyncStreamName::new("issues").unwrap();
        let mut a = BTreeMap::new();
        a.insert("workspace_id".to_string(), Value::String("W1".to_string()));
        let mut b = BTreeMap::new();
        b.insert("workspace_id".to_string(), Value::String("W2".to_string()));
        let key_a = local_stream_key(&stream, &a);
        let key_b = local_stream_key(&stream, &b);
        assert_ne!(key_a, key_b);
        // Both still start with the wire stream name (debuggability).
        assert!(key_a.as_str().starts_with("issues__params_"));
        assert!(key_b.as_str().starts_with("issues__params_"));
    }

    #[test]
    fn local_stream_key_does_not_panic_for_max_length_stream_names() {
        // Regression: an `expect` in `local_stream_key` would fire when
        // `stream.len() + "__params_<16-hex>".len() > MAX_SYNC_TOKEN_LEN`.
        // Truncating the stream prefix at a char boundary keeps the
        // composite within budget without crashing.
        let long = "a".repeat(MAX_SYNC_TOKEN_LEN);
        let stream = SyncStreamName::new(long).unwrap();
        let mut params = BTreeMap::new();
        params.insert("k".to_string(), Value::String("v".to_string()));
        let key = local_stream_key(&stream, &params);
        assert!(key.as_str().len() <= MAX_SYNC_TOKEN_LEN);
        assert!(key.as_str().contains("__params_"));
    }

    #[test]
    fn local_stream_key_differs_for_long_stream_names_with_shared_prefix() {
        // Regression: a previous version hashed only the params, then
        // appended the truncated stream prefix. Two stream names that
        // shared the same first ~999 bytes would collapse to the same
        // composite key (cache cross-contamination at the local store).
        // The fix hashes the FULL stream + params so distinct streams
        // still get distinct keys even after prefix truncation.
        let prefix = "x".repeat(MAX_SYNC_TOKEN_LEN - 10);
        let stream_a = SyncStreamName::new(format!("{prefix}aaaaaaaaa")).unwrap();
        let stream_b = SyncStreamName::new(format!("{prefix}bbbbbbbbb")).unwrap();
        let mut params = BTreeMap::new();
        params.insert("k".to_string(), Value::String("v".to_string()));
        let key_a = local_stream_key(&stream_a, &params);
        let key_b = local_stream_key(&stream_b, &params);
        assert_ne!(
            key_a, key_b,
            "long stream names sharing a prefix must not collide"
        );
    }

    #[test]
    fn local_stream_key_is_stable_per_params() {
        // Determinism: the same params produce the same key across
        // calls (BTreeMap is sorted by key, so canonical JSON is
        // stable; FNV-1a has no random seed).
        let stream = SyncStreamName::new("issues").unwrap();
        let mut params = BTreeMap::new();
        params.insert("status".to_string(), serde_json::json!({"in": ["open"]}));
        params.insert("workspace_id".to_string(), Value::String("W".to_string()));
        let k1 = local_stream_key(&stream, &params);
        let k2 = local_stream_key(&stream, &params);
        assert_eq!(k1, k2);
    }

    #[test]
    fn sync_stream_tag_is_stable() {
        assert_eq!(
            sync_stream_tag("posts_for_tenant"),
            "sync:stream:posts_for_tenant"
        );
    }

    // ---- RFC 088 §C: stream_params_hash + sync_stream_params_tag ---
    //
    // These tests double as the cross-language hash spec. The
    // `stream_params_hash_golden_fixture` test pins a set of
    // (stream, params) → hex-hash pairs. Future ports (a Swift
    // client, a Go server, etc.) MUST reproduce these exact bytes
    // or the per-(stream, params) live-wakeup routing silently
    // breaks.

    #[test]
    fn stream_params_tag_format() {
        let tag = sync_stream_params_tag("issues", 0xABCD_1234_5678_9ABC);
        assert_eq!(tag, "sync:stream:issues:abcd123456789abc");
    }

    #[test]
    fn params_hash_hex_len_matches_formatter() {
        // The formatter in `sync_stream_params_tag` hard-codes
        // `{:016x}`. `PARAMS_HASH_HEX_LEN` must match — otherwise
        // the live validator (`pocopine_live::is_params_hash_suffix`)
        // would gate the wrong byte-count and reject valid topics
        // (or accept malformed ones).
        let tag = sync_stream_params_tag("s", 0);
        let suffix = tag.rsplit_once(':').unwrap().1;
        assert_eq!(suffix.len(), PARAMS_HASH_HEX_LEN);
    }

    #[test]
    fn stream_params_tag_pads_to_16_hex() {
        // Small hashes must still be 16 hex chars (zero-padded) so
        // the topic format is uniform across clients.
        let tag = sync_stream_params_tag("issues", 0xFFu64);
        assert_eq!(tag, "sync:stream:issues:00000000000000ff");
    }

    #[test]
    fn stream_params_hash_empty_params_distinguishes_streams() {
        let h_issues = stream_params_hash("issues", &StreamParams::new());
        let h_comments = stream_params_hash("comments", &StreamParams::new());
        assert_ne!(
            h_issues, h_comments,
            "stream name folds into the hash; empty params alone must not alias"
        );
    }

    #[test]
    fn stream_params_hash_separator_avoids_prefix_collision() {
        // "ab" + {} vs "a" + {"b": null}: without the separator
        // these would have identical byte sequences. With the 0x00
        // separator they don't.
        let mut p2 = StreamParams::new();
        p2.insert("b".into(), Value::Null);
        let h1 = stream_params_hash("ab", &StreamParams::new());
        let h2 = stream_params_hash("a", &p2);
        assert_ne!(h1, h2);
    }

    #[test]
    fn stream_params_hash_is_stable_across_insert_order() {
        // StreamParams is a BTreeMap — sorted-by-key by construction.
        // Inserting in different orders must produce identical hashes.
        let mut p_in_order = StreamParams::new();
        p_in_order.insert("a".into(), json!("1"));
        p_in_order.insert("b".into(), json!("2"));
        p_in_order.insert("c".into(), json!("3"));

        let mut p_reversed = StreamParams::new();
        p_reversed.insert("c".into(), json!("3"));
        p_reversed.insert("a".into(), json!("1"));
        p_reversed.insert("b".into(), json!("2"));

        assert_eq!(
            stream_params_hash("issues", &p_in_order),
            stream_params_hash("issues", &p_reversed),
        );
    }

    #[test]
    fn stream_params_hash_distinguishes_distinct_values() {
        let mut p1 = StreamParams::new();
        p1.insert("workspace_id".into(), json!("W1"));
        let mut p2 = StreamParams::new();
        p2.insert("workspace_id".into(), json!("W2"));
        assert_ne!(
            stream_params_hash("issues", &p1),
            stream_params_hash("issues", &p2),
        );
    }

    /// **Golden fixture** — DO NOT EDIT VALUES.
    ///
    /// These hex-encoded hashes are the wire-protocol contract for
    /// RFC 088 §C. Any port (server-side hash producer, client-side
    /// hash subscriber) must produce exactly these bytes for these
    /// inputs, or the live-wakeup topic routing breaks silently
    /// (mutations get published to topic X, clients listen on topic
    /// X', no overlap, no error).
    ///
    /// If a future change MUST alter the hash function, bump
    /// `LIVE_PROTOCOL_V1` first and add migration logic.
    #[test]
    #[allow(clippy::type_complexity)] // golden-fixture-only triple
    fn stream_params_hash_golden_fixture() {
        // (stream, params_kvs, expected_hex_hash)
        let cases: &[(&str, &[(&str, Value)], &str)] = &[
            // 1. bare stream, empty params
            ("issues", &[], "issues_empty"),
            // 2. single param, string value
            ("issues", &[("workspace_id", json!("W1"))], "issues_w1"),
            // 3. two params, sorted order
            (
                "issues",
                &[("status", json!("open")), ("workspace_id", json!("W1"))],
                "issues_w1_open",
            ),
            // 4. nested object value (rare but supported)
            (
                "comments",
                &[("filter", json!({"status": "active"}))],
                "comments_nested",
            ),
            // 5. distinct stream name with same params as #2
            ("comments", &[("workspace_id", json!("W1"))], "comments_w1"),
        ];

        let mut computed: Vec<(String, u64)> = Vec::new();
        for (stream, kvs, label) in cases {
            let mut params = StreamParams::new();
            for (k, v) in *kvs {
                params.insert((*k).to_string(), v.clone());
            }
            computed.push(((*label).into(), stream_params_hash(stream, &params)));
        }

        // The expected hashes are pinned by the snapshot below.
        // Pretty-printed so a diff in CI shows exactly which case
        // drifted.
        let snapshot: String = computed
            .iter()
            .map(|(label, hash)| format!("{label} = {hash:016x}"))
            .collect::<Vec<_>>()
            .join("\n");

        let expected = "\
issues_empty = ebb76190a8dc90b7
issues_w1 = 6937b67bfe60e730
issues_w1_open = 35eea32b30a1d59c
comments_nested = ba8db781db55988d
comments_w1 = 668b4b2f8b17847e";

        assert_eq!(
            snapshot, expected,
            "RFC 088 §C wire-protocol hash spec drifted. \
             If this drift is intentional, bump LIVE_PROTOCOL_V1 \
             FIRST and add migration logic — old clients hashing \
             the old way will MISS wakeups until they upgrade."
        );
    }
}
