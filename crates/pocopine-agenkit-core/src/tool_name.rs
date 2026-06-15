//! Tool-name sanitization and a collision-safe, bijective name map shared by the
//! provider crates (RFC-093 §D5).
//!
//! Model APIs (OpenAI Chat Completions, Anthropic Messages) require tool names
//! to match `^[a-zA-Z0-9_-]{1,64}$`. An id that doesn't (e.g. `weather.lookup`)
//! is sanitized on the way out and reversed on the way back. Sanitization is
//! lossy, so two distinct ids can collide (`weather.lookup` and `weather/lookup`
//! both → `weather_lookup`); [`ToolNameMap`] disambiguates a request's tools so
//! every one keeps a distinct wire name and a model tool call always reverses to
//! exactly one original id.
//!
//! Both providers previously carried byte-identical copies of this logic; it
//! lives here so a fix (or the collision rule) can never drift between them.

use std::collections::HashMap;

use crate::ToolDescriptor;

/// Maximum wire-name length both APIs accept.
const MAX_LEN: usize = 64;

/// Map every byte outside `[A-Za-z0-9_-]` to `_`, cap at [`MAX_LEN`], and never
/// return empty. Lossy: distinct ids can collide — use [`ToolNameMap`] when a
/// set of tools must keep distinct wire names.
pub fn sanitize_tool_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Every retained char is ASCII (1 byte), so a byte truncation at MAX_LEN is
    // always a char boundary.
    out.truncate(MAX_LEN);
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// A bijective tool-name map for one request's tools: original id ⇄ wire name.
/// Sanitized names that collide are disambiguated deterministically (`-2`, `-3`,
/// …, re-capped at 64) in tool order, so the same tool set always yields the
/// same map — letting the request side (forward) and the response side (reverse)
/// build independent copies that agree.
#[derive(Debug, Default, Clone)]
pub struct ToolNameMap {
    forward: HashMap<String, String>,
    reverse: HashMap<String, String>,
}

impl ToolNameMap {
    /// Build the map for a request's tool descriptors. Order-stable.
    pub fn from_descriptors(tools: &[ToolDescriptor]) -> Self {
        let mut map = Self::default();
        for tool in tools {
            map.insert(&tool.id);
        }
        map
    }

    fn insert(&mut self, id: &str) {
        // A duplicate id is degenerate; keep the first mapping.
        if self.forward.contains_key(id) {
            return;
        }
        let base = sanitize_tool_name(id);
        let mut wire = base.clone();
        let mut n = 2u32;
        while self.reverse.contains_key(&wire) {
            // Collision with a *different* id: append `-n`, re-capping at 64.
            let suffix = format!("-{n}");
            let keep = MAX_LEN.saturating_sub(suffix.len());
            let mut candidate = base.clone();
            candidate.truncate(keep);
            candidate.push_str(&suffix);
            wire = candidate;
            n += 1;
        }
        self.forward.insert(id.to_string(), wire.clone());
        self.reverse.insert(wire, id.to_string());
    }

    /// The wire name an original tool id is sent under. Falls back to sanitizing
    /// an id not in the map (e.g. one referenced only in a replayed history).
    pub fn wire(&self, id: &str) -> String {
        self.forward
            .get(id)
            .cloned()
            .unwrap_or_else(|| sanitize_tool_name(id))
    }

    /// The original id for a wire name the model emitted; identity when unknown
    /// (no tools declared, or an already-valid name with no mapping).
    pub fn resolve(&self, wire: &str) -> String {
        self.reverse
            .get(wire)
            .cloned()
            .unwrap_or_else(|| wire.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_conforms_to_the_name_pattern() {
        assert_eq!(sanitize_tool_name("search_docs"), "search_docs");
        assert_eq!(sanitize_tool_name("a-b_C9"), "a-b_C9");
        assert_eq!(sanitize_tool_name("weather.lookup"), "weather_lookup");
        assert_eq!(sanitize_tool_name("my tool/v2"), "my_tool_v2");
        assert_eq!(sanitize_tool_name("café"), "caf_");
        assert_eq!(sanitize_tool_name(&"x".repeat(100)).len(), 64);
        assert_eq!(sanitize_tool_name(""), "_");
    }

    #[test]
    fn map_reverses_an_already_sanitized_name() {
        let map = ToolNameMap::from_descriptors(&[ToolDescriptor::new("weather.lookup", "w")]);
        assert_eq!(map.wire("weather.lookup"), "weather_lookup");
        assert_eq!(map.resolve("weather_lookup"), "weather.lookup");
        // Identity for an unknown wire name.
        assert_eq!(map.resolve("other"), "other");
    }

    #[test]
    fn colliding_ids_get_distinct_wire_names_that_reverse_correctly() {
        // Both sanitize to `weather_lookup`; the map must keep them distinct and
        // reverse each wire name to its own original id (no silent mis-dispatch).
        let map = ToolNameMap::from_descriptors(&[
            ToolDescriptor::new("weather.lookup", "a"),
            ToolDescriptor::new("weather/lookup", "b"),
        ]);
        let first = map.wire("weather.lookup");
        let second = map.wire("weather/lookup");
        assert_eq!(first, "weather_lookup");
        assert_ne!(first, second, "colliding ids must get distinct wire names");
        assert_eq!(map.resolve(&first), "weather.lookup");
        assert_eq!(map.resolve(&second), "weather/lookup");
    }

    #[test]
    fn disambiguation_respects_the_length_cap() {
        let long_a = format!("{}.x", "z".repeat(70));
        let long_b = format!("{}/x", "z".repeat(70));
        let map = ToolNameMap::from_descriptors(&[
            ToolDescriptor::new(long_a.clone(), "a"),
            ToolDescriptor::new(long_b.clone(), "b"),
        ]);
        assert!(map.wire(&long_a).len() <= 64);
        assert!(map.wire(&long_b).len() <= 64);
        assert_ne!(map.wire(&long_a), map.wire(&long_b));
    }
}
