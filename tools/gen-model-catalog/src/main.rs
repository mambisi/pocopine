//! Codegen the agenkit model catalog from LiteLLM's
//! `model_prices_and_context_window.json`.
//!
//! ```text
//! cargo run -p gen-model-catalog -- [--input <path>] [--url <url>] [--out-dir <dir>]
//! ```
//!
//! With no `--input`, the latest LiteLLM JSON is fetched (via `curl`, so the
//! tool keeps zero Rust HTTP deps); `--input <file>` reads a vendored/pinned
//! copy. It writes two generated files into the catalog dir — `id.rs` (the
//! `Model` descriptors) and `models.rs` (the typed `models::*` handles) —
//! consumed by `pocopine_agenkit::server::catalog`. Host-only dev tool — never
//! built for wasm (see the workspace `metadata.hosts` list).
//!
//! Refresh workflow (e.g. a scheduled CI job that opens a PR):
//! ```text
//! curl -fsSL <litellm-url> -o /tmp/litellm.json
//! cargo run -p gen-model-catalog -- --input /tmp/litellm.json
//! cargo fmt && cargo test -p pocopine-agenkit
//! ```

use std::fs;
use std::process::ExitCode;

use serde_json::{Map, Value};

const DEFAULT_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const DEFAULT_OUT_DIR: &str = "crates/pocopine-agenkit/src/server/catalog";
const PROVIDERS: &[&str] = &["anthropic", "openai", "qwen"];

/// One mapped catalog entry (provider/model with our normalized fields).
struct Entry {
    id: String,
    context_window: u32,
    max_output: u32,
    reasoning: bool,
    vision: bool,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_creation: f64,
}

fn main() -> ExitCode {
    let mut input: Option<String> = None;
    let mut url = DEFAULT_URL.to_string();
    let mut out_dir = DEFAULT_OUT_DIR.to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => input = args.next(),
            "--url" => {
                if let Some(v) = args.next() {
                    url = v;
                }
            }
            "--out-dir" => {
                if let Some(v) = args.next() {
                    out_dir = v;
                }
            }
            other => {
                eprintln!("unknown arg: {other}");
                return ExitCode::FAILURE;
            }
        }
    }

    let raw = match &input {
        Some(path) => match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) => {
                eprintln!("read {path}: {err}");
                return ExitCode::FAILURE;
            }
        },
        None => match fetch(&url) {
            Ok(raw) => raw,
            Err(err) => {
                eprintln!("fetch {url}: {err}");
                return ExitCode::FAILURE;
            }
        },
    };

    let data: Value = match serde_json::from_str(&raw) {
        Ok(data) => data,
        Err(err) => {
            eprintln!("parse LiteLLM JSON: {err}");
            return ExitCode::FAILURE;
        }
    };
    let Some(obj) = data.as_object() else {
        eprintln!("expected a top-level JSON object");
        return ExitCode::FAILURE;
    };

    let mut entries: Vec<Entry> = obj
        .iter()
        .filter_map(|(key, v)| map_entry(key, v))
        .collect();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    entries.dedup_by(|a, b| a.id == b.id);

    // Each model's id string lives in exactly one place — its `models::*` const;
    // the descriptor list references it. One generated file holds both.
    let consts = const_assignments(&entries);
    let out = format!("{out_dir}/generated.rs");
    if let Err(err) = fs::write(&out, render(&entries, &consts)) {
        eprintln!("write {out}: {err}");
        return ExitCode::FAILURE;
    }
    eprintln!("wrote {} models to {out}", entries.len());
    ExitCode::SUCCESS
}

/// Map one LiteLLM entry to our normalized [`Entry`], or `None` to skip it.
fn map_entry(key: &str, v: &Value) -> Option<Entry> {
    let m = v.as_object()?;
    let raw_provider = m.get("litellm_provider").and_then(Value::as_str)?;
    let provider = normalized_provider(raw_provider)?;
    if m.get("mode").and_then(Value::as_str) != Some("chat") {
        return None;
    }
    if key.contains("ft:") || key.contains(":free") {
        return None;
    }

    let input = num(m, "input_cost_per_token")?;
    let context_window = u32_from(num_u(m, "max_input_tokens").or_else(|| num_u(m, "max_tokens"))?);
    if context_window == 0 {
        return None;
    }
    let raw_out = num_u(m, "max_output_tokens")
        .or_else(|| num_u(m, "max_tokens"))
        .unwrap_or(context_window as u64);
    let max_output = match u32_from(raw_out) {
        0 => context_window,
        n => n.min(context_window),
    };

    // Normalize the id to `provider/model` (strip a redundant leading prefix).
    let provider_prefix = format!("{provider}/");
    let raw_provider_prefix = format!("{raw_provider}/");
    let model = key
        .strip_prefix(&provider_prefix)
        .or_else(|| key.strip_prefix(&raw_provider_prefix))
        .unwrap_or(key);

    Some(Entry {
        id: format!("{provider}/{model}"),
        context_window,
        max_output,
        reasoning: m
            .get("supports_reasoning")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        vision: m
            .get("supports_vision")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        input: per_mtok(input),
        output: per_mtok(num(m, "output_cost_per_token").unwrap_or(0.0)),
        cache_read: per_mtok(num(m, "cache_read_input_token_cost").unwrap_or(0.0)),
        cache_creation: per_mtok(num(m, "cache_creation_input_token_cost").unwrap_or(0.0)),
    })
}

fn normalized_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("anthropic"),
        "openai" => Some("openai"),
        // LiteLLM catalogs Alibaba Cloud Model Studio / Qwen compatible-mode
        // rows under the provider name used on the wire.
        "dashscope" | "qwen" => Some("qwen"),
        _ => None,
    }
}

/// The `@generated` banner shared by both output files.
fn banner() -> String {
    let mut s = String::new();
    s.push_str("// @generated by gen-model-catalog from LiteLLM's\n");
    s.push_str("// model_prices_and_context_window.json. DO NOT EDIT — regenerate via\n");
    s.push_str(
        "// `cargo run -p gen-model-catalog`. Source: github.com/BerriAI/litellm (MIT).\n\n",
    );
    s
}

/// Render `catalog/generated.rs`: the `Model` descriptors plus the typed
/// `models::*` const handles. Each descriptor `id` references its const, so the
/// alias string lives in exactly one place.
fn render(entries: &[Entry], consts: &[(String, String)]) -> String {
    let mut s = banner();
    s.push_str("use super::catalog::{Model, ModelPricing};\n\n");
    s.push_str("/// Models derived from the upstream LiteLLM price/capability data. Each `id`\n");
    s.push_str("/// references the matching `models::*` const, so the alias lives in one place.\n");
    s.push_str("#[rustfmt::skip]\n");
    s.push_str("pub(super) fn generated() -> Vec<Model> {\n    vec![\n");
    for (e, (provider, name)) in entries.iter().zip(consts) {
        s.push_str(&format!(
            "        Model {{ id: models::{provider}::{name}, context_window: {}, max_output: {}, \
             reasoning: {}, vision: {}, pricing: ModelPricing {{ input: {}, output: {}, \
             cache_read: {}, cache_creation: {} }} }},\n",
            e.context_window,
            e.max_output,
            e.reasoning,
            e.vision,
            lit(e.input),
            lit(e.output),
            lit(e.cache_read),
            lit(e.cache_creation),
        ));
    }
    s.push_str("    ]\n}\n\n");
    render_models(entries, consts, &mut s);
    s
}

/// Assign each entry a `(provider, const_name)`. Names are unique within a
/// provider — a (rare) sanitization collision gets a numeric suffix — so every
/// model has exactly one handle and the descriptor list can reference it.
fn const_assignments(entries: &[Entry]) -> Vec<(String, String)> {
    use std::collections::HashSet;
    let mut seen: std::collections::HashMap<&str, HashSet<String>> =
        std::collections::HashMap::new();
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let (provider, model) = e.id.split_once('/').expect("entry id is provider/model");
        let base = const_name(model);
        let taken = seen.entry(provider).or_default();
        let mut name = base.clone();
        let mut n = 2;
        while taken.contains(&name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        taken.insert(name.clone());
        out.push((provider.to_string(), name));
    }
    out
}

/// Emit the typed `ModelRef` const handles into the `models` module —
/// `models::<provider>::<NAME>`. Grouped by provider; generated from the same
/// data as the descriptors, so they never drift.
fn render_models(entries: &[Entry], consts: &[(String, String)], s: &mut String) {
    s.push_str("/// Typed handles for the known models, grouped by provider.\n");
    s.push_str("///\n");
    s.push_str("/// `models::anthropic::CLAUDE_OPUS_4_8` is a `const` [`ModelRef`]; pass it\n");
    s.push_str("/// anywhere a `ModelRef` is expected. An open-weight model behind a gateway\n");
    s.push_str("/// the catalog doesn't index still uses `ModelRef::new(\"provider/model\")`.\n");
    s.push_str("#[rustfmt::skip]\n");
    s.push_str("pub mod models {\n");
    s.push_str("    use pocopine_agenkit_core::ModelRef;\n");

    // Stable provider order; entries are already sorted by id.
    for &provider in PROVIDERS {
        let group: Vec<(&Entry, &str)> = entries
            .iter()
            .zip(consts)
            .filter(|(_, (p, _))| p == provider)
            .map(|(e, (_, name))| (e, name.as_str()))
            .collect();
        if group.is_empty() {
            continue;
        }
        s.push_str(&format!(
            "\n    /// {provider} model handles.\n    pub mod {provider} {{\n"
        ));
        s.push_str("        use super::ModelRef;\n");
        for (e, name) in group {
            let mut tags = vec![format!("{} ctx", human(e.context_window))];
            if e.reasoning {
                tags.push("reasoning".to_string());
            }
            if e.vision {
                tags.push("vision".to_string());
            }
            s.push_str(&format!("        /// `{}` · {}\n", e.id, tags.join(" · ")));
            s.push_str(&format!(
                "        pub const {name}: ModelRef = ModelRef::from_static({:?});\n",
                e.id
            ));
        }
        s.push_str("    }\n");
    }
    s.push_str("}\n");
}

/// Sanitize a model id into a SCREAMING_SNAKE_CASE Rust const ident.
fn const_name(model: &str) -> String {
    let mut out = String::new();
    let mut prev_us = false;
    for ch in model.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    let trimmed = out.trim_matches('_');
    // Rust idents can't start with a digit.
    match trimmed.chars().next() {
        Some(c) if c.is_ascii_digit() => format!("M_{trimmed}"),
        Some(_) => trimmed.to_string(),
        None => "MODEL".to_string(),
    }
}

/// Render a token count compactly (`200K`, `1M`) for a doc comment.
fn human(n: u32) -> String {
    if n >= 1_000_000 && n.is_multiple_of(1_000_000) {
        format!("{}M", n / 1_000_000)
    } else if n >= 1000 && n.is_multiple_of(1000) {
        format!("{}K", n / 1000)
    } else {
        n.to_string()
    }
}

fn fetch(url: &str) -> Result<String, String> {
    let out = std::process::Command::new("curl")
        .args(["-fsSL", url])
        .output()
        .map_err(|e| format!("run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl exited {}", out.status));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("utf8: {e}"))
}

fn num(m: &Map<String, Value>, k: &str) -> Option<f64> {
    m.get(k).and_then(Value::as_f64)
}

fn num_u(m: &Map<String, Value>, k: &str) -> Option<u64> {
    m.get(k)
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
}

fn u32_from(v: u64) -> u32 {
    v.min(u32::MAX as u64) as u32
}

/// LiteLLM prices are per token; ours are per million tokens. Round to 6 decimals
/// so binary float noise doesn't litter the generated literals.
fn per_mtok(per_token: f64) -> f64 {
    (per_token * 1_000_000.0 * 1_000_000.0).round() / 1_000_000.0
}

/// Format a value as a valid `f64` literal (always carries a decimal/exponent).
fn lit(v: f64) -> String {
    format!("{v:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_supported_chat_providers_with_mtok_conversion() {
        let json = serde_json::json!({
            "gpt-4o": {
                "litellm_provider": "openai", "mode": "chat",
                "max_input_tokens": 128000, "max_output_tokens": 16384,
                "input_cost_per_token": 2.5e-6, "output_cost_per_token": 1e-5,
                "cache_read_input_token_cost": 1.25e-6, "supports_vision": true
            },
            "dashscope/qwen-plus": {
                "litellm_provider": "dashscope", "mode": "chat",
                "max_input_tokens": 131072, "max_output_tokens": 8192,
                "input_cost_per_token": 4e-7, "output_cost_per_token": 1.2e-6
            },
            "claude-x": {
                "litellm_provider": "anthropic", "mode": "chat",
                "max_input_tokens": 200000, "max_output_tokens": 64000,
                "input_cost_per_token": 3e-6, "output_cost_per_token": 1.5e-5,
                "cache_read_input_token_cost": 3e-7, "cache_creation_input_token_cost": 3.75e-6
            },
            "embed": { "litellm_provider": "openai", "mode": "embedding",
                       "input_cost_per_token": 1e-7 }
        });
        let obj = json.as_object().unwrap();
        let mut got: Vec<Entry> = obj.iter().filter_map(|(k, v)| map_entry(k, v)).collect();
        got.sort_by(|a, b| a.id.cmp(&b.id));

        assert_eq!(got.len(), 3, "embedding model is filtered out");

        let claude = &got[0];
        assert_eq!(claude.id, "anthropic/claude-x");
        assert_eq!(claude.context_window, 200_000);
        assert!((claude.input - 3.0).abs() < 1e-9); // 3e-6/token → $3/Mtok
        assert!((claude.cache_creation - 3.75).abs() < 1e-9);

        let gpt = &got[1];
        assert_eq!(gpt.id, "openai/gpt-4o");
        assert!((gpt.input - 2.5).abs() < 1e-9);
        assert!((gpt.output - 10.0).abs() < 1e-9);
        assert!((gpt.cache_read - 1.25).abs() < 1e-9);
        assert!(gpt.vision && !gpt.reasoning);

        let qwen = &got[2];
        assert_eq!(qwen.id, "qwen/qwen-plus");
        assert_eq!(qwen.context_window, 131_072);
        assert!((qwen.input - 0.4).abs() < 1e-9);
        assert!((qwen.output - 1.2).abs() < 1e-9);
    }

    #[test]
    fn id_prefix_is_not_doubled() {
        let json = serde_json::json!({
            "openai/gpt-4o": { "litellm_provider": "openai", "mode": "chat",
                "max_input_tokens": 128000, "max_output_tokens": 16384,
                "input_cost_per_token": 2.5e-6 },
            "dashscope/qwen-plus": { "litellm_provider": "dashscope", "mode": "chat",
                "max_input_tokens": 129024, "max_output_tokens": 16384,
                "input_cost_per_token": 4e-7 }
        });
        let obj = json.as_object().unwrap();
        let mut got: Vec<Entry> = obj.iter().filter_map(|(k, v)| map_entry(k, v)).collect();
        got.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(got[0].id, "openai/gpt-4o");
        assert_eq!(got[1].id, "qwen/qwen-plus");
    }

    #[test]
    fn const_name_sanitizes_and_avoids_leading_digit() {
        assert_eq!(const_name("gpt-4o"), "GPT_4O");
        assert_eq!(const_name("claude-opus-4-8"), "CLAUDE_OPUS_4_8");
        assert_eq!(const_name("qwen-plus"), "QWEN_PLUS");
        assert_eq!(const_name("o1-preview"), "O1_PREVIEW");
        assert_eq!(const_name("gpt-4.1"), "GPT_4_1");
        // A model id starting with a digit gets an `M_` prefix to stay a valid ident.
        assert_eq!(const_name("3-mini"), "M_3_MINI");
    }

    #[test]
    fn render_models_emits_const_handles_per_provider() {
        let entries = vec![
            Entry {
                id: "anthropic/claude-opus-4-8".to_string(),
                context_window: 200_000,
                max_output: 64_000,
                reasoning: true,
                vision: true,
                input: 15.0,
                output: 75.0,
                cache_read: 1.5,
                cache_creation: 18.75,
            },
            Entry {
                id: "openai/gpt-4o".to_string(),
                context_window: 128_000,
                max_output: 16_384,
                reasoning: false,
                vision: true,
                input: 2.5,
                output: 10.0,
                cache_read: 1.25,
                cache_creation: 0.0,
            },
            Entry {
                id: "qwen/qwen-plus".to_string(),
                context_window: 131_072,
                max_output: 8192,
                reasoning: false,
                vision: false,
                input: 0.4,
                output: 1.2,
                cache_read: 0.0,
                cache_creation: 0.0,
            },
        ];
        let consts = const_assignments(&entries);
        let file = render(&entries, &consts);
        // The `models` module carries the typed const handles, per provider.
        assert!(file.contains("pub mod models {"));
        assert!(file.contains("pub mod anthropic {"));
        assert!(file.contains(
            "pub const CLAUDE_OPUS_4_8: ModelRef = ModelRef::from_static(\"anthropic/claude-opus-4-8\");"
        ));
        assert!(file.contains("pub mod openai {"));
        assert!(
            file.contains("pub const GPT_4O: ModelRef = ModelRef::from_static(\"openai/gpt-4o\");")
        );
        assert!(file.contains("pub mod qwen {"));
        assert!(file.contains(
            "pub const QWEN_PLUS: ModelRef = ModelRef::from_static(\"qwen/qwen-plus\");"
        ));
        // Doc comment carries the human-readable spec.
        assert!(file.contains("`anthropic/claude-opus-4-8` · 200K ctx · reasoning · vision"));

        // The descriptors reference the const, not a repeated literal.
        assert!(file.contains("Model { id: models::openai::GPT_4O,"));
        assert!(file.contains("Model { id: models::anthropic::CLAUDE_OPUS_4_8,"));
        assert!(file.contains("Model { id: models::qwen::QWEN_PLUS,"));
        // The id literal appears exactly once (in the const).
        assert_eq!(file.matches("\"openai/gpt-4o\"").count(), 1);
        assert_eq!(file.matches("\"qwen/qwen-plus\"").count(), 1);
    }

    #[test]
    fn const_assignments_suffix_a_sanitization_collision() {
        // Two ids that sanitize to the same ident get distinct const names.
        let entries = vec![
            Entry {
                id: "openai/gpt-4o".to_string(),
                context_window: 1,
                max_output: 1,
                reasoning: false,
                vision: false,
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_creation: 0.0,
            },
            Entry {
                id: "openai/gpt.4o".to_string(),
                context_window: 1,
                max_output: 1,
                reasoning: false,
                vision: false,
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_creation: 0.0,
            },
        ];
        let names: Vec<String> = const_assignments(&entries)
            .into_iter()
            .map(|(_, n)| n)
            .collect();
        assert_eq!(names, vec!["GPT_4O".to_string(), "GPT_4O_2".to_string()]);
    }
}
