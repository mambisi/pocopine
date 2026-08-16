//! Generation-model registry: image/video models the chat catalog cannot hold.
//!
//! These models (Seedream image, Seedance video) are **tool territory**
//! (RFC-122 §4): they never enter the chat loop, so none of the chat
//! catalog's gates apply. What an app's `image.generate` / `video.generate`
//! tool still wants is a typed list — which ids exist, what inputs they take,
//! whether a batch/stream is possible — so model choice isn't a stringly
//! affair in every tool.
//!
//! **Hand-authored, deliberately.** The chat catalog is generated from
//! LiteLLM's data; LiteLLM carries **no** ByteDance generation rows under a
//! volcengine/byteplus provider (checked 2026-08-16), and per-image /
//! per-second pricing doesn't fit the token-priced [`Model`](super::Model)
//! shape. Until an upstream machine-readable source exists, this file is the
//! list — ids from the BytePlus ModelArk docs. If LiteLLM grows these rows,
//! fold this into `gen-model-catalog` and delete the hand list.
//!
//! Ark ids often carry a release-date suffix (`seedream-4-5-251128`);
//! [`lookup_generation`] matches exact ids first, then the longest
//! `-`-boundary prefix, so a dated alias resolves to its family entry.

use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ModelRef};

/// What a generation model produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationKind {
    /// Still images (Seedream family).
    Image,
    /// Video (Seedance family).
    Video,
}

/// One image/video generation model an app tool can offer.
#[derive(Clone, Debug)]
pub struct GenerationModel {
    /// The `"provider/model"` alias — a family prefix when Ark releases carry
    /// date suffixes (see [`lookup_generation`]).
    pub id: ModelRef,
    /// What it produces.
    pub kind: GenerationKind,
    /// Accepts image input (image-to-image editing / image-to-video) — the
    /// RFC-122 §2.1 chaining input.
    pub image_input: bool,
    /// Declared ceiling for one request's batch (`sequential_image_generation`
    /// on Seedream) — maps to a §5.3 group's `expected`. `None` = one output
    /// per request.
    pub sequential_images: Option<u32>,
    /// Whether the provider streams each completed output as it finishes
    /// (Seedream SSE) — each event is a complete output, captured as its own
    /// artifact, not a preview chunk.
    pub streaming: bool,
}

/// The registry (BytePlus ModelArk families).
pub fn generation_models() -> &'static [GenerationModel] {
    static MODELS: std::sync::OnceLock<Vec<GenerationModel>> = std::sync::OnceLock::new();
    MODELS.get_or_init(|| {
        vec![
            GenerationModel {
                id: ModelRef::from_static("byteplus/seedream-4-5"),
                kind: GenerationKind::Image,
                image_input: true,
                sequential_images: Some(15),
                streaming: true,
            },
            GenerationModel {
                // Per ModelArk docs: 5.0 **pro** supports neither
                // `sequential_image_generation` nor `stream`.
                id: ModelRef::from_static("byteplus/seedream-5-0-pro"),
                kind: GenerationKind::Image,
                image_input: true,
                sequential_images: None,
                streaming: false,
            },
            GenerationModel {
                id: ModelRef::from_static("byteplus/seedream-5-0-lite"),
                kind: GenerationKind::Image,
                image_input: true,
                sequential_images: Some(15),
                streaming: true,
            },
            GenerationModel {
                id: ModelRef::from_static("byteplus/seedance-1-5-pro-251215"),
                kind: GenerationKind::Video,
                image_input: true,
                sequential_images: None,
                streaming: false,
            },
            GenerationModel {
                id: ModelRef::from_static("byteplus/dreamina-seedance-2-0-260128"),
                kind: GenerationKind::Video,
                image_input: true,
                sequential_images: None,
                streaming: false,
            },
            // Seedance 2.5 is announced on ModelArk; add its id here once the
            // exact string is confirmed against the docs/console.
        ]
    })
}

/// Resolve an alias to its registry entry: exact id first, then the longest
/// entry whose id is a `-`-boundary prefix of the alias — so
/// `byteplus/seedream-4-5-251128` (a dated Ark release) resolves to the
/// `byteplus/seedream-4-5` family. `None` degrades exactly like the chat
/// catalog: the tool decides.
pub fn lookup_generation(model: &ModelRef) -> Option<&'static GenerationModel> {
    let alias = model.as_str();
    let mut best: Option<&'static GenerationModel> = None;
    for entry in generation_models() {
        let id = entry.id.as_str();
        if id == alias {
            return Some(entry);
        }
        let dated_family = alias
            .strip_prefix(id)
            .is_some_and(|rest| rest.starts_with('-'));
        if dated_family && best.is_none_or(|b| id.len() > b.id.as_str().len()) {
            best = Some(entry);
        }
    }
    best
}

/// Knobs for an image-generation tool call, shaped on what the grounded
/// provider (Seedream) actually takes. Serde + `JsonSchema` so an app tool
/// embeds it directly in its typed input:
///
/// ```ignore
/// #[derive(Deserialize, schemars::JsonSchema)]
/// struct ImageGenIn {
///     prompt: String,
///     #[serde(default, flatten)]
///     config: ImageGenerationConfig,
/// }
/// ```
///
/// Per-call *inputs* stay out on purpose: the prompt and any source-image
/// artifact refs (RFC-122 §2.1 chaining → §2.2 `derived_from` lineage) are
/// the tool's own fields, so lineage is declared explicitly, never smuggled
/// through config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct ImageGenerationConfig {
    /// Output size — a provider preset (`"1K"`, `"2K"`, `"4K"`) or exact
    /// dimensions (`"2048x2048"`). `None` ⇒ the provider default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// Batch generation: produce a consistent set of up to this many images
    /// (Seedream `sequential_image_generation`). Bounded by the registry's
    /// `sequential_images` ceiling — [`validate`](Self::validate) enforces
    /// it. Maps to an RFC-122 §5.3 group's `expected`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_images: Option<u32>,
    /// Stream each completed image as it finishes (SSE). Requires a model
    /// the registry marks `streaming`; each event is a complete image the
    /// tool captures as its own artifact.
    pub stream: bool,
    /// Provider watermarking (the provider default is on).
    pub watermark: bool,
    /// Deterministic seed, when reproducibility matters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
}

impl Default for ImageGenerationConfig {
    fn default() -> Self {
        Self {
            size: None,
            max_images: None,
            stream: false,
            watermark: true,
            seed: None,
        }
    }
}

impl ImageGenerationConfig {
    /// Reject a config the resolved model cannot honor, before any provider
    /// call (work-or-loud): a batch on a model without sequential support, a
    /// batch over the model's ceiling, streaming where unsupported, or an
    /// image config aimed at a video model. An alias the registry doesn't
    /// index passes — the provider decides.
    pub fn validate(&self, model: &ModelRef) -> AgenkitResult<()> {
        let Some(entry) = lookup_generation(model) else {
            return Ok(());
        };
        if entry.kind != GenerationKind::Image {
            return Err(AgenkitError::config(format!(
                "model `{model}` generates {:?}, not images",
                entry.kind
            )));
        }
        if let Some(requested) = self.max_images {
            match entry.sequential_images {
                None => {
                    return Err(AgenkitError::config(format!(
                        "model `{model}` does not support batch generation \
                         (max_images = {requested} requested)"
                    )));
                }
                Some(ceiling) if requested > ceiling || requested == 0 => {
                    return Err(AgenkitError::config(format!(
                        "model `{model}` supports 1..={ceiling} images per \
                         request (max_images = {requested} requested)"
                    )));
                }
                Some(_) => {}
            }
        }
        if self.stream && !entry.streaming {
            return Err(AgenkitError::config(format!(
                "model `{model}` does not stream generation output"
            )));
        }
        Ok(())
    }
}

/// Knobs for a video-generation tool call, shaped on what the grounded
/// provider (Seedance, async task API) actually takes. Same embedding story
/// as [`ImageGenerationConfig`]; the prompt and any first-frame source-image
/// artifact ref (image-to-video, §2.1) are the tool's own input fields.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct VideoGenerationConfig {
    /// Clip length in seconds. `None` ⇒ the provider default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u32>,
    /// Output resolution preset (`"480p"`, `"720p"`, `"1080p"`). `None` ⇒
    /// the provider default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Aspect ratio (`"16:9"`, `"9:16"`, `"1:1"`, `"adaptive"` — adaptive
    /// derives from the source image on image-to-video). `None` ⇒ the
    /// provider default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<String>,
    /// Hold the camera fixed (no provider-invented camera motion).
    pub camera_fixed: bool,
    /// Provider watermarking (the provider default is on).
    pub watermark: bool,
    /// Deterministic seed, when reproducibility matters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
}

impl Default for VideoGenerationConfig {
    fn default() -> Self {
        Self {
            duration_seconds: None,
            resolution: None,
            ratio: None,
            camera_fixed: false,
            watermark: true,
            seed: None,
        }
    }
}

impl VideoGenerationConfig {
    /// Reject a config the resolved model cannot honor (see
    /// [`ImageGenerationConfig::validate`]): today that is aiming a video
    /// config at a non-video model. An unindexed alias passes.
    pub fn validate(&self, model: &ModelRef) -> AgenkitResult<()> {
        let Some(entry) = lookup_generation(model) else {
            return Ok(());
        };
        if entry.kind != GenerationKind::Video {
            return Err(AgenkitError::config(format!(
                "model `{model}` generates {:?}, not video",
                entry.kind
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_dated_aliases_resolve_to_the_family() {
        let exact = lookup_generation(&ModelRef::new("byteplus/seedream-5-0-pro")).unwrap();
        assert_eq!(exact.kind, GenerationKind::Image);
        assert!(!exact.streaming);

        // A dated Ark release id resolves to its family entry.
        let dated = lookup_generation(&ModelRef::new("byteplus/seedream-4-5-251128")).unwrap();
        assert_eq!(dated.id.as_str(), "byteplus/seedream-4-5");
        assert_eq!(dated.sequential_images, Some(15));
        assert!(dated.streaming);
    }

    #[test]
    fn prefix_matching_respects_segment_boundaries() {
        // Not a dash-boundary continuation: must NOT match seedream-4-5.
        assert!(lookup_generation(&ModelRef::new("byteplus/seedream-4-55")).is_none());
        assert!(lookup_generation(&ModelRef::new("qwen/wanx-v1")).is_none());
    }

    #[test]
    fn video_models_accept_image_input_for_chaining() {
        // The §2.1 image→video chain requires i2v input on the video entries.
        for entry in generation_models() {
            if entry.kind == GenerationKind::Video {
                assert!(entry.image_input, "{} must accept image input", entry.id);
            }
        }
    }

    #[test]
    fn image_config_validates_against_the_registry() {
        let seedream_45 = ModelRef::new("byteplus/seedream-4-5-251128");
        let pro_50 = ModelRef::new("byteplus/seedream-5-0-pro");

        // Defaults pass everywhere an image model resolves.
        let config = ImageGenerationConfig::default();
        assert!(config.validate(&seedream_45).is_ok());
        assert!(config.validate(&pro_50).is_ok());

        // Batch + stream fit 4.5 but not 5.0-pro (no sequential, no stream).
        let batch = ImageGenerationConfig {
            max_images: Some(4),
            stream: true,
            ..ImageGenerationConfig::default()
        };
        assert!(batch.validate(&seedream_45).is_ok());
        let err = batch.validate(&pro_50).unwrap_err();
        assert!(err.to_string().contains("batch"), "{err}");

        // Over the declared ceiling fails loudly.
        let over = ImageGenerationConfig {
            max_images: Some(16),
            ..ImageGenerationConfig::default()
        };
        assert!(over.validate(&seedream_45).is_err());

        // Aiming an image config at a video model is a config error; an
        // alias the registry doesn't index passes (the provider decides).
        let video = ModelRef::new("byteplus/seedance-1-5-pro-251215");
        assert!(config.validate(&video).is_err());
        assert!(batch.validate(&ModelRef::new("other/imagegen-x")).is_ok());
    }

    #[test]
    fn video_config_validates_kind_only() {
        let config = VideoGenerationConfig {
            duration_seconds: Some(5),
            resolution: Some("1080p".to_string()),
            ..VideoGenerationConfig::default()
        };
        assert!(
            config
                .validate(&ModelRef::new("byteplus/dreamina-seedance-2-0-260128"))
                .is_ok()
        );
        let err = config
            .validate(&ModelRef::new("byteplus/seedream-4-5-251128"))
            .unwrap_err();
        assert!(err.to_string().contains("not video"), "{err}");
        assert!(config.validate(&ModelRef::new("other/videogen-x")).is_ok());
    }

    #[test]
    fn configs_round_trip_serde_with_partial_json() {
        // Tool args arrive as partial JSON: absent fields take defaults
        // (watermark stays on), and defaults serialize compactly.
        let config: ImageGenerationConfig =
            serde_json::from_str(r#"{"size":"2K","max_images":4}"#).unwrap();
        assert!(config.watermark);
        assert!(!config.stream);
        assert_eq!(config.max_images, Some(4));

        let json = serde_json::to_string(&ImageGenerationConfig::default()).unwrap();
        assert!(!json.contains("size"), "{json}");
        let back: ImageGenerationConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ImageGenerationConfig::default());

        let video: VideoGenerationConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(video, VideoGenerationConfig::default());
    }
}
