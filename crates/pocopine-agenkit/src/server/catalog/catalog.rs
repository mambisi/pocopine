//! The hand-written catalog core: the [`Model`]/[`ModelPricing`] descriptors and
//! the [`lookup`]/[`all`] accessors. The data itself is generated (see
//! [`super::generated`] for the descriptors and the typed `models` handles).

use std::sync::OnceLock;

use pocopine_agenkit_core::{CostEstimate, ModelRef, Usage};

/// Per-million-token prices, in US dollars.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelPricing {
    /// Uncached input (prompt) tokens, USD per 1M.
    pub input: f64,
    /// Output (completion) tokens, USD per 1M.
    pub output: f64,
    /// Cached input read, USD per 1M (cache *hit*).
    pub cache_read: f64,
    /// Cache write/creation, USD per 1M (0 where the provider doesn't bill it).
    pub cache_creation: f64,
}

/// Static metadata for one model, resolved from a [`ModelRef`] alias.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Model {
    /// The `"provider/model"` alias this entry describes. A [`ModelRef`] so a
    /// generated entry borrows a `'static` literal (no allocation), while a
    /// config-loaded one owns its alias.
    pub id: ModelRef,
    /// Maximum input context, in tokens.
    pub context_window: u32,
    /// Maximum output tokens the model will produce in one response.
    pub max_output: u32,
    /// Whether the model emits/uses reasoning ("thinking") content.
    pub reasoning: bool,
    /// Whether the model accepts image input.
    pub vision: bool,
    /// Token prices.
    pub pricing: ModelPricing,
}

impl Model {
    /// Estimate the USD cost of a call from its token [`Usage`] — uncached
    /// input, output, cache-read, and cache-creation tokens each priced at their
    /// own per-Mtok rate (`Usage.input_tokens` excludes the cached subset).
    pub fn estimate_cost(&self, usage: &Usage) -> CostEstimate {
        let p = &self.pricing;
        let amount = (usage.input_tokens as f64 * p.input
            + usage.output_tokens as f64 * p.output
            + usage.cache_read_tokens as f64 * p.cache_read
            + usage.cache_creation_tokens as f64 * p.cache_creation)
            / 1_000_000.0;
        CostEstimate::new("USD", amount)
    }
}

/// The catalog, lazily built from the generated (LiteLLM-derived) descriptors.
/// There is no hand-built fallback: the generated data is committed to the repo
/// (git-tracked), so it is always present — a model it doesn't list resolves to
/// `None` and cost/context features degrade gracefully.
fn catalog() -> &'static [Model] {
    static CATALOG: OnceLock<Vec<Model>> = OnceLock::new();
    CATALOG.get_or_init(super::generated::generated)
}

/// All known models, in catalog order.
pub fn all() -> &'static [Model] {
    catalog()
}

/// Resolve a [`ModelRef`] to its [`Model`] descriptor, if known.
///
/// Matches the full `"provider/model"` alias first, then falls back to the
/// model portion (so a bare `"gpt-4o"` resolves the same entry as
/// `"openai/gpt-4o"`). Returns `None` for an unlisted alias — callers degrade.
pub fn lookup(model: &ModelRef) -> Option<&'static Model> {
    let by_alias = catalog().iter().find(|m| m.id == *model);
    if by_alias.is_some() {
        return by_alias;
    }
    let wanted = model.model();
    catalog().iter().find(|m| m.id.model() == wanted)
}

#[cfg(test)]
mod tests {
    use super::super::models;
    use super::*;

    #[test]
    fn every_entry_is_internally_consistent() {
        for m in all() {
            assert!(
                m.id.as_str().contains('/'),
                "id should be provider/model: {}",
                m.id
            );
            assert!(m.context_window > 0, "{} context_window", m.id);
            assert!(
                m.max_output > 0 && m.max_output <= m.context_window,
                "{} max_output {} vs window {}",
                m.id,
                m.max_output,
                m.context_window
            );
            let p = &m.pricing;
            for (label, v) in [
                ("input", p.input),
                ("output", p.output),
                ("cache_read", p.cache_read),
                ("cache_creation", p.cache_creation),
            ] {
                assert!(v >= 0.0 && v.is_finite(), "{} pricing.{label} = {v}", m.id);
            }
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<&str> = all().iter().map(|m| m.id.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate model ids in catalog");
    }

    #[test]
    fn generated_data_is_loaded_with_real_prices() {
        // The generated (LiteLLM-derived) catalog is large and carries real
        // prices — gpt-4o's uncached input is $2.50/Mtok.
        assert!(
            all().len() > 100,
            "expected generated entries, got {}",
            all().len()
        );
        let gpt4o = lookup(&models::openai::GPT_4O).expect("gpt-4o");
        assert_eq!(gpt4o.pricing.input, 2.5);
    }

    #[test]
    fn lookup_resolves_our_models() {
        for id in [
            "anthropic/claude-opus-4-8",
            "anthropic/claude-sonnet-4-6",
            "openai/gpt-4o",
        ] {
            let m = lookup(&ModelRef::new(id)).unwrap_or_else(|| panic!("{id} should resolve"));
            assert!(m.context_window >= 100_000, "{id} ctx {}", m.context_window);
        }
    }

    #[test]
    fn typed_model_handles_resolve_in_the_catalog() {
        // A generated const handle is a drop-in `ModelRef` and resolves to its
        // descriptor exactly like the equivalent string alias.
        let handle = &models::anthropic::CLAUDE_OPUS_4_8;
        assert_eq!(handle.as_str(), "anthropic/claude-opus-4-8");
        assert_eq!(lookup(handle).map(|m| m.id.as_str()), Some(handle.as_str()));
        assert_eq!(
            lookup(&models::openai::GPT_4O).map(|m| m.id.as_str()),
            Some("openai/gpt-4o")
        );
    }

    #[test]
    fn lookup_falls_back_to_model_portion() {
        // a bare model name resolves to the same entry as the full alias
        let bare = lookup(&ModelRef::new("gpt-4o")).expect("known by model portion");
        assert_eq!(bare.id.as_str(), "openai/gpt-4o");
    }

    #[test]
    fn lookup_misses_unknown() {
        assert!(lookup(&ModelRef::new("acme/does-not-exist")).is_none());
    }

    #[test]
    fn estimate_cost_prices_every_token_class() {
        // Price-agnostic (prices refresh from LiteLLM): 1M tokens of each class
        // costs exactly that class's per-Mtok price, summed.
        let m = lookup(&models::anthropic::CLAUDE_SONNET_4_6).unwrap();
        let p = m.pricing;
        let cost =
            m.estimate_cost(&Usage::new(1_000_000, 1_000_000).with_cache(1_000_000, 1_000_000));
        let expected = p.input + p.output + p.cache_read + p.cache_creation;
        assert_eq!(cost.currency, "USD");
        assert!((cost.amount - expected).abs() < 1e-9, "got {}", cost.amount);
    }
}
