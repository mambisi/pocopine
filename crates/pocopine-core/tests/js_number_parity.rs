//! RFC-099 Phase 1 — correctness fuzz for the shared JS number
//! formatter (`js_number::to_js_string`).
//!
//! The SSR "same text for same value" invariant is satisfied **by
//! construction** — the wasm client and the SSR host both format
//! through this one function — so this test does NOT compare against
//! the V8 engine (which differs only in last-digit tie-break for rare
//! 16-digit ties; see `js_number.rs`). Instead it proves the two
//! properties that matter:
//!
//!   1. **Round-trip**: every formatted finite `f64` re-parses to the
//!      exact same value — the digits are always sufficient (no
//!      precision loss), over a 2M random-bit-pattern corpus.
//!   2. **JS notation shape** (exponent thresholds, Infinity/NaN, -0)
//!      is pinned by the unit table in `js_number.rs`.
//!
//! Host test — no wasm/JS needed.

use pocopine_core::js_number::to_js_string;

#[test]
fn every_finite_format_round_trips() {
    // Deterministic LCG over u64 → f64::from_bits, so any failure
    // replays from its seed.
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut checked = 0u64;
    for _ in 0..2_000_000 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let n = f64::from_bits(seed);
        if !n.is_finite() {
            continue; // Infinity / NaN have no numeric round-trip; see unit table.
        }
        let s = to_js_string(n);
        let back: f64 = s
            .parse()
            .unwrap_or_else(|e| panic!("unparseable {s:?} for bits={seed:#018x}: {e}"));
        // Value equality (not bits): `String(-0)` is "0", which
        // round-trips to +0.0 — JS-correct, and `0.0 == -0.0`.
        assert!(
            back == n,
            "round-trip failed: n={n} (bits={seed:#018x}) -> {s:?} -> {back}"
        );
        checked += 1;
    }
    assert!(
        checked > 1_000_000,
        "fuzz ran too few finite cases: {checked}"
    );
}
