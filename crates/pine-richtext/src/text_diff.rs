//! Minimal single-span text diff for incremental DOM text patching.
//!
//! The reconciler's text fast path turns one text node's old content into its
//! new content by rewriting only the changed span rather than the whole node,
//! so a keystroke in a large paragraph is O(edit), not O(paragraph). This module
//! is the pure, DOM-free core of that patch — kept out of the `view` feature so
//! it unit- and property-tests off-target (no browser).

/// Compute the minimal single contiguous edit that turns `old` into `new`,
/// expressed as `(offset, count, replacement)` in **UTF-16 code units** — the
/// unit [`web_sys::CharacterData::replace_data`] takes.
///
/// The diff runs over `char`s (Unicode scalar values), so a surrogate pair is
/// never split — swapping one astral char for another (e.g. `😀`→`😁`, which
/// share a high surrogate) replaces the whole char, not a half.
///
/// Applying it — `old[..offset] ++ replacement ++ old[offset + count..]` over
/// the UTF-16 units — reproduces `new` exactly. `old == new` yields an empty
/// edit (`count == 0`, `replacement == ""`).
pub(crate) fn text_splice(old: &str, new: &str) -> (u32, u32, String) {
    let old_c: Vec<char> = old.chars().collect();
    let new_c: Vec<char> = new.chars().collect();
    let common = old_c.len().min(new_c.len());

    let mut prefix = 0;
    while prefix < common && old_c[prefix] == new_c[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < common - prefix
        && old_c[old_c.len() - 1 - suffix] == new_c[new_c.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let u16_len = |chars: &[char]| -> u32 { chars.iter().map(|c| c.len_utf16() as u32).sum() };
    let offset = u16_len(&old_c[..prefix]);
    let count = u16_len(&old_c[prefix..old_c.len() - suffix]);
    let replacement: String = new_c[prefix..new_c.len() - suffix].iter().collect();
    (offset, count, replacement)
}

#[cfg(test)]
mod tests {
    use super::text_splice;

    /// Mirror `CharacterData::replace_data(offset, count, replacement)` over the
    /// UTF-16 units, asserting the result stays valid UTF-16 (no split pair).
    fn apply(old: &str, offset: u32, count: u32, replacement: &str) -> String {
        let mut units: Vec<u16> = old.encode_utf16().collect();
        let start = offset as usize;
        let end = start + count as usize;
        units.splice(start..end, replacement.encode_utf16());
        String::from_utf16(&units).expect("splice kept valid UTF-16 (no surrogate split)")
    }

    fn roundtrip(old: &str, new: &str) {
        let (offset, count, replacement) = text_splice(old, new);
        assert_eq!(
            apply(old, offset, count, &replacement),
            new,
            "text_splice({old:?} -> {new:?}) = ({offset}, {count}, {replacement:?})"
        );
    }

    #[test]
    fn append() {
        roundtrip("hello", "hello!");
    }
    #[test]
    fn prepend() {
        roundtrip("world", "hello world");
    }
    #[test]
    fn mid_insert() {
        roundtrip("hello world", "hello big world");
    }
    #[test]
    fn delete_middle() {
        roundtrip("hello big world", "hello world");
    }
    #[test]
    fn delete_end() {
        roundtrip("hello!", "hello");
    }
    #[test]
    fn replace_middle() {
        roundtrip("abcXYZdef", "abcQdef");
    }
    #[test]
    fn full_replace() {
        roundtrip("abc", "xyz");
    }
    #[test]
    fn empty_and_full() {
        roundtrip("", "hello");
        roundtrip("hello", "");
        roundtrip("", "");
    }
    #[test]
    fn repeated_chars() {
        roundtrip("aaa", "aaaa");
        roundtrip("aaaa", "aaa");
        roundtrip("aXa", "aa");
    }
    #[test]
    fn shared_prefix_and_suffix() {
        roundtrip("the cat sat", "the dog sat");
    }
    #[test]
    fn no_change_is_empty_edit() {
        let (offset, count, replacement) = text_splice("same", "same");
        assert_eq!((count, replacement.as_str()), (0, ""));
        assert_eq!(offset, "same".encode_utf16().count() as u32);
    }
    #[test]
    fn emoji_insert_before() {
        roundtrip("a😀b", "a😀Xb");
    }
    #[test]
    fn emoji_delete() {
        roundtrip("a😀b", "ab");
    }
    #[test]
    fn emoji_swap_shares_high_surrogate() {
        // 😀 U+1F600 and 😁 U+1F601 share the D83D high surrogate — a u16-level
        // diff would split the pair here; the char-level diff replaces the whole
        // char. `apply` panics if the result isn't valid UTF-16.
        roundtrip("a😀b", "a😁b");
        roundtrip("😀😀", "😀😁");
    }

    /// For thousands of random `(old, new)` pairs over an alphabet with astral
    /// chars, applying the splice must reproduce `new` and keep valid UTF-16.
    #[test]
    fn property_random_pairs_reproduce_new() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        let alphabet = ['a', 'b', 'c', ' ', '😀', '😁', 'é', '中'];
        for _ in 0..8000 {
            let old_len = next() % 12;
            let new_len = next() % 12;
            let old: String = (0..old_len).map(|_| alphabet[next() % alphabet.len()]).collect();
            let new: String = (0..new_len).map(|_| alphabet[next() % alphabet.len()]).collect();
            let (offset, count, replacement) = text_splice(&old, &new);
            assert_eq!(
                apply(&old, offset, count, &replacement),
                new,
                "old={old:?} new={new:?} -> ({offset}, {count}, {replacement:?})"
            );
        }
    }
}
