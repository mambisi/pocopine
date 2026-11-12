//! The hand-written catalog core: the [`Model`]/[`ModelPricing`] descriptors and
//! the [`lookup`]/[`all`] accessors. The data itself is generated (see
//! [`super::generated`] for the descriptors and the typed `models` handles).

use std::sync::{OnceLock, PoisonError, RwLock};

use pocopine_agenkit_core::{AgenkitError, AgenkitResult, CostEstimate, ModelRef, Usage};

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
///
/// The capability flags describe the **model** (per the upstream LiteLLM
/// data); what the **transport** supports (streaming, strict `json_schema`,
/// the tool wire) is
/// [`ProviderCapabilities`](crate::server::ProviderCapabilities) on the
/// provider. The runtime gates on both: a capability needs a model that has it
/// and a wire that carries it. The post-`vision` flags default to `false` when
/// deserializing, so a config-loaded entry written before they existed stays
/// valid.
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
    /// Whether the model supports native tool/function calling.
    #[serde(default)]
    pub tools: bool,
    /// Whether the provider offers built-in web search for this model (e.g.
    /// DashScope `enable_search`, OpenAI search-preview models).
    #[serde(default)]
    pub web_search: bool,
    /// Whether the model accepts audio input.
    #[serde(default)]
    pub audio_input: bool,
    /// Whether the model can produce audio output.
    #[serde(default)]
    pub audio_output: bool,
    /// Whether the model can produce image output (RFC-122 §4.1). Consulted
    /// at request build: an image-output model with no `ArtifactSink` wired
    /// is a config error, and returned image bytes are captured through the
    /// sink rather than ever riding the transcript.
    #[serde(default)]
    pub image_output: bool,
    /// Whether the model accepts PDF/document input.
    #[serde(default)]
    pub pdf_input: bool,
    /// Whether the model supports native schema-constrained structured output
    /// (`response_schema` / strict `json_schema`).
    #[serde(default)]
    pub structured_output: bool,
    /// Token prices.
    pub pricing: ModelPricing,
}

impl Model {
    /// A model with only its sizes known, otherwise treated like a model the
    /// catalog does not list: nothing is refused on its account (`tools`,
    /// `vision` and `structured_output` on, so the provider decides), nothing
    /// is added for it (`reasoning` and `image_output` off), and it has no
    /// prices (cost estimates read zero). Set what you know with struct update
    /// syntax: `Model { reasoning: true, ..Model::new("ollama/qwen3", 32_768, 8_192) }`.
    pub fn new(id: impl Into<ModelRef>, context_window: u32, max_output: u32) -> Self {
        Self {
            id: id.into(),
            context_window,
            max_output,
            reasoning: false,
            vision: true,
            tools: true,
            web_search: false,
            audio_input: false,
            audio_output: false,
            image_output: false,
            pdf_input: false,
            structured_output: true,
            pricing: ModelPricing {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_creation: 0.0,
            },
        }
    }

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

/// Models registered at runtime ([`register`]), one per alias. Each
/// descriptor is leaked so [`lookup`] keeps handing out `&'static Model`.
fn registered() -> &'static RwLock<Vec<&'static Model>> {
    static REGISTERED: OnceLock<RwLock<Vec<&'static Model>>> = OnceLock::new();
    REGISTERED.get_or_init(|| RwLock::new(Vec::new()))
}

/// All models in the generated catalog, in catalog order. Models added with
/// [`register`] are not listed here; [`lookup`] finds them.
pub fn all() -> &'static [Model] {
    catalog()
}

/// Describe a model the generated catalog does not list (a gateway's model, a
/// local one), or replace what it says about one, for this process. Every
/// [`lookup`] sees it from then on: compaction and in-turn trimming size the
/// context window from it, cost metering prices with it, and providers read
/// its capability flags.
///
/// Meant for startup or a periodic refresh of a provider's listing: each
/// registration with new values keeps its descriptor for the life of the
/// process (a few hundred bytes), so references already handed out stay
/// valid; registering the same values again keeps nothing new.
///
/// Errors (config) for an alias without a provider part, a zero context
/// window, or an output cap of zero or above the window.
pub fn register(model: Model) -> AgenkitResult<()> {
    if !model.id.as_str().contains('/') {
        return Err(AgenkitError::config(format!(
            "model `{}` is not a \"provider/model\" alias",
            model.id
        )));
    }
    if model.context_window == 0 || model.max_output == 0 || model.max_output > model.context_window
    {
        return Err(AgenkitError::config(format!(
            "model `{}`: a context window of {} cannot hold {} output tokens",
            model.id, model.context_window, model.max_output
        )));
    }
    let mut list = registered().write().unwrap_or_else(PoisonError::into_inner);
    match list.iter_mut().find(|m| m.id == model.id) {
        Some(slot) if **slot == model => {}
        Some(slot) => *slot = Box::leak(Box::new(model)),
        None => list.push(Box::leak(Box::new(model))),
    }
    Ok(())
}

/// Resolve a [`ModelRef`] to its [`Model`] descriptor, if known.
///
/// Matches the full `"provider/model"` alias first, a [`register`]ed model
/// before the generated one, then falls back to the model portion (so a bare
/// `"gpt-4o"` resolves the same entry as `"openai/gpt-4o"`), the generated
/// catalog first: registering `"proxy/gpt-4o"` changes what that alias
/// resolves to, not every bare `"gpt-4o"`. Returns `None` for an unlisted
/// alias — callers degrade.
pub fn lookup(model: &ModelRef) -> Option<&'static Model> {
    let registered = registered().read().unwrap_or_else(PoisonError::into_inner);
    let exact = |m: &&Model| m.id == *model;
    if let Some(found) = registered.iter().copied().find(exact) {
        return Some(found);
    }
    if let Some(found) = catalog().iter().find(exact) {
        return Some(found);
    }
    let wanted = model.model();
    let portion = |m: &&Model| m.id.model() == wanted;
    catalog()
        .iter()
        .find(portion)
        .or_else(|| registered.iter().copied().find(portion))
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
    fn capability_flags_survive_generation() {
        // Anchors that won't regress upstream: Claude Opus does tools, vision,
        // PDF input, and structured output; gpt-3.5-turbo does tools but has no
        // vision or audio.
        let opus = lookup(&models::anthropic::CLAUDE_OPUS_4_8).expect("opus 4.8");
        assert!(opus.tools && opus.vision && opus.pdf_input && opus.structured_output);
        let turbo = lookup(&models::openai::GPT_3_5_TURBO).expect("gpt-3.5-turbo");
        assert!(turbo.tools && !turbo.vision && !turbo.audio_input && !turbo.audio_output);
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
            "qwen/qwen-plus",
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
        assert_eq!(
            lookup(&models::qwen::QWEN_PLUS).map(|m| m.id.as_str()),
            Some("qwen/qwen-plus")
        );
    }

    #[test]
    fn lookup_falls_back_to_model_portion() {
        // a bare model name resolves to the same entry as the full alias
        let bare = lookup(&ModelRef::new("gpt-4o")).expect("known by model portion");
        assert_eq!(bare.id.as_str(), "openai/gpt-4o");
    }

    #[test]
    fn a_registered_model_is_found_and_can_be_replaced() {
        let id = ModelRef::new("test-gateway/registered-model-a");
        assert!(lookup(&id).is_none());
        register(Model {
            reasoning: true,
            ..Model::new(id.clone(), 32_768, 4_096)
        })
        .unwrap();
        let first = lookup(&id).expect("registered");
        assert_eq!((first.context_window, first.reasoning), (32_768, true));
        // By its model portion too, like a generated entry.
        let bare = lookup(&ModelRef::new("registered-model-a")).unwrap();
        assert_eq!(bare.id, id);

        // The same values again keep the same descriptor.
        register(Model {
            reasoning: true,
            ..Model::new(id.clone(), 32_768, 4_096)
        })
        .unwrap();
        assert!(std::ptr::eq(first, lookup(&id).unwrap()));

        // New values replace it; the old reference stays valid.
        register(Model::new(id.clone(), 65_536, 4_096)).unwrap();
        assert_eq!(lookup(&id).unwrap().context_window, 65_536);
        assert_eq!(first.context_window, 32_768);
    }

    #[test]
    fn a_registered_model_wins_over_the_generated_one() {
        // A gateway serving a generated alias with a smaller window.
        let id = ModelRef::new("test-proxy/gpt-4o");
        register(Model::new(id.clone(), 8_192, 1_024)).unwrap();
        assert_eq!(lookup(&id).unwrap().context_window, 8_192);
        // The generated alias, and a bare name, still resolve to their own.
        assert_eq!(
            lookup(&models::openai::GPT_4O).unwrap().id,
            models::openai::GPT_4O
        );
        assert_eq!(
            lookup(&ModelRef::new("gpt-4o")).unwrap().id,
            models::openai::GPT_4O
        );
    }

    #[test]
    fn a_model_registered_for_its_size_refuses_nothing() {
        use crate::server::GenerateRequest;
        use pocopine_agenkit_core::{
            Content, ContentPart, MediaPart, Message, Role, ToolDescriptor,
        };
        let id = ModelRef::new("test-local/sized-only");
        let image = Message::new(
            Role::User,
            Content::from_parts(vec![ContentPart::Media(MediaPart {
                media_type: "image/png".to_string(),
                url: Some("https://example.com/a.png".to_string()),
                data_base64: None,
                name: None,
            })]),
        );
        let request = GenerateRequest {
            model: id.clone(),
            messages: vec![image],
            tools: vec![ToolDescriptor::new("fs.read", "Read a file")],
            ..GenerateRequest::default()
        };
        // As an unknown model: the provider decides.
        assert!(request.ensure_tool_support().is_ok());
        assert!(request.ensure_media_support().is_ok());
        register(Model::new(id.clone(), 32_768, 4_096)).unwrap();
        assert!(request.ensure_tool_support().is_ok());
        assert!(request.ensure_media_support().is_ok());
        let sized = lookup(&id).unwrap();
        assert!(!sized.reasoning && !sized.image_output);
    }

    #[test]
    fn register_rejects_a_model_that_cannot_be_sized() {
        assert!(register(Model::new("no-provider-part", 8_192, 1_024)).is_err());
        assert!(register(Model::new("test/zero-window", 0, 0)).is_err());
        assert!(register(Model::new("test/output-too-big", 4_096, 8_192)).is_err());
        assert!(lookup(&ModelRef::new("test/zero-window")).is_none());
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
