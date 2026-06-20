//! RFC-099 Phase 1 — pure-Rust, JS-identical number → string.
//!
//! Reproduces JavaScript `Number.prototype.toString(10)` / `String(n)`
//! exactly, so the SSR host renderer and the wasm client agree on every
//! number's text — RFC-099's "same text for same value" parity
//! invariant. Used by [`crate::text::js_number_string`] on **both**
//! targets (no more `js_sys` on wasm, no more host approximation) and,
//! later, by the SSR host expression backend.
//!
//! Algorithm: ECMA-262 `Number::toString` (§6.1.6.1.20). The shortest
//! round-trip significant digits come from Rust's `{:e}` formatting
//! (a Ryū-class shortest-round-trip), and then JS's digit-placement /
//! exponent rules are applied.
//!
//! **Tie-break note.** For the rare value whose shortest round-trip is
//! a *tie* (two equal-length decimals both recover the same `f64` —
//! e.g. `667082108456853.3` vs `.2`), Rust's formatter and V8 may pick
//! different last digits. Both are correct (both round-trip); they
//! differ only in tie convention. This is fine **by construction**:
//! after RFC-099 the wasm client and the SSR host BOTH format through
//! this one function, so they always agree — matching V8's exact
//! tie-break is therefore a non-goal. The hard guarantee is that every
//! output round-trips (digits are sufficient) and uses JS's notation.

/// Format `n` exactly as JavaScript `String(n)` would.
pub fn to_js_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    // Covers both `+0.0` and `-0.0` — JS `String(-0)` is `"0"`.
    if n == 0.0 {
        return "0".to_string();
    }
    if n.is_infinite() {
        return if n < 0.0 { "-Infinity" } else { "Infinity" }.to_string();
    }

    let neg = n < 0.0;
    let abs = n.abs();

    // Shortest round-trip scientific form: "<d>[.<frac>]e<exp>", one
    // digit before the point, exponent in base 10. Rust's `{:e}` is
    // shortest-round-trip, so the significant digits equal V8's.
    let sci = format!("{abs:e}");
    let (mantissa, exp_str) = sci
        .split_once('e')
        .expect("`{:e}` output always contains an exponent");
    let exp: i32 = exp_str.parse().expect("`{:e}` exponent is an integer");

    // `s` = significant digits (no `.`), `k` = digit count. With the
    // `{:e}` form, ECMA's `n` (the decimal-point position such that the
    // value is `s × 10^(n − k)`) is `exp + 1`.
    let s: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = s.len() as i32;
    let point = exp + 1;

    let mut out = String::new();
    if neg {
        out.push('-');
    }

    if k <= point && point <= 21 {
        // Integer: digits then `point − k` trailing zeros.  (e.g. 100)
        out.push_str(&s);
        out.push_str(&"0".repeat((point - k) as usize));
    } else if 0 < point && point <= 21 {
        // Decimal point inside the digits.  (e.g. 12.5, 123.456)
        out.push_str(&s[..point as usize]);
        out.push('.');
        out.push_str(&s[point as usize..]);
    } else if -6 < point && point <= 0 {
        // Leading `0.` then `-point` zeros then the digits. (e.g. 0.001)
        out.push_str("0.");
        out.push_str(&"0".repeat((-point) as usize));
        out.push_str(&s);
    } else {
        // Exponential: `point > 21` or `point ≤ −6`.  (e.g. 1e+21, 1e-7)
        let e = point - 1;
        if k == 1 {
            out.push_str(&s);
        } else {
            out.push_str(&s[..1]);
            out.push('.');
            out.push_str(&s[1..]);
        }
        out.push('e');
        out.push(if e >= 0 { '+' } else { '-' });
        out.push_str(&e.abs().to_string());
    }

    out
}

#[cfg(test)]
mod tests {
    use super::to_js_string;

    // Each pair is `(value, exactly what JS String(value) produces)`.
    #[test]
    fn matches_javascript_string_semantics() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-1.0, "-1"),
            (100.0, "100"),
            (12.5, "12.5"),
            (-12.5, "-12.5"),
            (123.456, "123.456"),
            (0.5, "0.5"),
            (0.001, "0.001"),
            (0.0000001, "1e-7"),    // 1e-7 → exponential (point ≤ −6)
            (0.000001, "0.000001"), // 1e-6 → decimal (point = −5)
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"), // point > 21 → exponential
            (1e-21, "1e-21"),
            (9007199254740992.0, "9007199254740992"), // 2^53
            (1234567890123456789.0, "1234567890123456800"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (f64::NAN, "NaN"),
            (1.5e300, "1.5e+300"),
            (5e-324, "5e-324"), // smallest subnormal
        ];
        for (n, want) in cases {
            assert_eq!(&to_js_string(*n), want, "to_js_string({n})");
        }
    }
}
