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

use pocopine_agenkit_core::ModelRef;

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
}
