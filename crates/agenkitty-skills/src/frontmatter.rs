//! `SKILL.md` frontmatter parsing + validation.
//!
//! Implements the agentskills.io field contract in full (RFC-121 § Format
//! contract) and parses the Claude Code extension fields into
//! [`ClaudeExt`] without ever acting on them. Violations of spec'd fields are
//! `Error` diagnostics (the skill is excluded); extension oddities degrade to
//! `Warning`/`Info`. Unknown fields are preserved in `raw` and never error.

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;
use serde_norway::Value as YamlValue;

use crate::meta::{ClaudeExt, ForkHint, Severity, SkillMeta};

/// A diagnostic before the discovery layer attaches the skill directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingDiagnostic {
    pub severity: Severity,
    pub rule: &'static str,
    pub message: String,
}

impl PendingDiagnostic {
    fn error(rule: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            rule,
            message: message.into(),
        }
    }

    fn warning(rule: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            rule,
            message: message.into(),
        }
    }

    fn info(rule: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            rule,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ParsedSkillFile {
    /// `None` when an `Error` diagnostic excludes the skill.
    pub meta: Option<SkillMeta>,
    pub ext: ClaudeExt,
    /// Full frontmatter as JSON values (string-keyed; non-string YAML keys are
    /// stringified) for tooling round-trips.
    pub raw: BTreeMap<String, JsonValue>,
    /// The Markdown body after the closing `---`, unmodified.
    pub body: String,
    pub diagnostics: Vec<PendingDiagnostic>,
}

/// Parse one `SKILL.md` source against the directory name it lives in.
pub(crate) fn parse_skill_file(dir_name: &str, src: &str) -> ParsedSkillFile {
    let mut diagnostics = Vec::new();

    let (yaml_src, body) = match split_frontmatter(src) {
        Ok(parts) => parts,
        Err(message) => {
            diagnostics.push(PendingDiagnostic::error("frontmatter.missing", message));
            return ParsedSkillFile {
                meta: None,
                ext: ClaudeExt::default(),
                raw: BTreeMap::new(),
                body: String::new(),
                diagnostics,
            };
        }
    };

    let mapping: BTreeMap<String, YamlValue> = match serde_norway::from_str::<YamlValue>(yaml_src) {
        Ok(YamlValue::Mapping(mapping)) => mapping
            .into_iter()
            .map(|(key, value)| (yaml_key_to_string(&key), value))
            .collect(),
        Ok(YamlValue::Null) => BTreeMap::new(),
        Ok(_) => {
            diagnostics.push(PendingDiagnostic::error(
                "frontmatter.shape",
                "frontmatter must be a YAML mapping",
            ));
            BTreeMap::new()
        }
        Err(err) => {
            diagnostics.push(PendingDiagnostic::error(
                "frontmatter.parse",
                format!("invalid YAML frontmatter: {err}"),
            ));
            BTreeMap::new()
        }
    };

    let raw: BTreeMap<String, JsonValue> = mapping
        .iter()
        .map(|(key, value)| (key.clone(), yaml_to_json(value)))
        .collect();

    let has_frontmatter_error = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error);

    let meta = if has_frontmatter_error {
        None
    } else {
        parse_standard_fields(dir_name, &mapping, &mut diagnostics)
    };
    let ext = parse_extension_fields(&mapping, &mut diagnostics);

    ParsedSkillFile {
        meta,
        ext,
        raw,
        body: body.to_string(),
        diagnostics,
    }
}

/// Split `---\n<yaml>\n---\n<body>`. The opening marker must be the first
/// line; the closing marker is the next line that is exactly `---`.
fn split_frontmatter(src: &str) -> Result<(&str, &str), String> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let rest = src
        .strip_prefix("---\r\n")
        .or_else(|| src.strip_prefix("---\n"))
        .ok_or_else(|| "SKILL.md must start with a `---` frontmatter block".to_string())?;

    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return Ok((yaml, body));
        }
        offset += line.len();
    }
    Err("frontmatter block is not terminated by a closing `---` line".to_string())
}

fn parse_standard_fields(
    dir_name: &str,
    mapping: &BTreeMap<String, YamlValue>,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> Option<SkillMeta> {
    let mut ok = true;

    let name = match mapping.get("name").and_then(YamlValue::as_str) {
        Some(name) => {
            let name = name.to_string();
            for (rule, message) in validate_name(&name, dir_name) {
                diagnostics.push(PendingDiagnostic::error(rule, message));
                ok = false;
            }
            name
        }
        None => {
            diagnostics.push(PendingDiagnostic::error(
                "name.missing",
                "frontmatter must carry a string `name`",
            ));
            ok = false;
            String::new()
        }
    };

    let description = match mapping.get("description").and_then(YamlValue::as_str) {
        Some(description) => {
            let description = description.to_string();
            let count = description.trim().chars().count();
            if count == 0 {
                diagnostics.push(PendingDiagnostic::error(
                    "description.empty",
                    "`description` must be non-empty",
                ));
                ok = false;
            } else if description.chars().count() > 1024 {
                diagnostics.push(PendingDiagnostic::error(
                    "description.length",
                    "`description` must be at most 1024 characters",
                ));
                ok = false;
            }
            description
        }
        None => {
            diagnostics.push(PendingDiagnostic::error(
                "description.missing",
                "frontmatter must carry a string `description`",
            ));
            ok = false;
            String::new()
        }
    };

    let license = string_field(mapping, "license", diagnostics);

    let compatibility = string_field(mapping, "compatibility", diagnostics);
    if let Some(compatibility) = &compatibility {
        let count = compatibility.chars().count();
        if count == 0 || count > 500 {
            diagnostics.push(PendingDiagnostic::error(
                "compatibility.length",
                "`compatibility` must be 1-500 characters when present",
            ));
            ok = false;
        }
    }

    let metadata = parse_metadata(mapping.get("metadata"), diagnostics);
    let allowed_tools = tool_list(mapping.get("allowed-tools"), "allowed-tools", diagnostics);

    if ok {
        Some(SkillMeta {
            name,
            description,
            license,
            compatibility,
            metadata,
            allowed_tools,
        })
    } else {
        None
    }
}

/// Every `name` rule from the spec, returned together so a bad name reports
/// all of its problems at once.
fn validate_name(name: &str, dir_name: &str) -> Vec<(&'static str, String)> {
    let mut violations = Vec::new();
    let count = name.chars().count();
    if count == 0 || count > 64 {
        violations.push(("name.length", "`name` must be 1-64 characters".to_string()));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        violations.push((
            "name.charset",
            "`name` may only contain lowercase letters, digits, and hyphens".to_string(),
        ));
    }
    if name.starts_with('-') || name.ends_with('-') {
        violations.push((
            "name.hyphen-edge",
            "`name` must not start or end with a hyphen".to_string(),
        ));
    }
    if name.contains("--") {
        violations.push((
            "name.hyphen-run",
            "`name` must not contain consecutive hyphens".to_string(),
        ));
    }
    if !name.is_empty() && name != dir_name {
        violations.push((
            "name.dir-mismatch",
            format!("`name` is `{name}` but the skill directory is `{dir_name}`"),
        ));
    }
    violations
}

fn parse_metadata(
    value: Option<&YamlValue>,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> BTreeMap<String, String> {
    let Some(value) = value else {
        return BTreeMap::new();
    };
    let YamlValue::Mapping(mapping) = value else {
        diagnostics.push(PendingDiagnostic::warning(
            "metadata.shape",
            "`metadata` must be a map; dropped",
        ));
        return BTreeMap::new();
    };
    let mut metadata = BTreeMap::new();
    for (key, value) in mapping {
        let key = yaml_key_to_string(key);
        match value {
            YamlValue::String(text) => {
                metadata.insert(key, text.clone());
            }
            // Coerce bare scalars (authors write `version: 1.0`) with a note.
            YamlValue::Bool(flag) => {
                diagnostics.push(PendingDiagnostic::warning(
                    "metadata.value-coerced",
                    format!("`metadata.{key}` is not a string; coerced"),
                ));
                metadata.insert(key, flag.to_string());
            }
            YamlValue::Number(number) => {
                diagnostics.push(PendingDiagnostic::warning(
                    "metadata.value-coerced",
                    format!("`metadata.{key}` is not a string; coerced"),
                ));
                metadata.insert(key, number.to_string());
            }
            _ => {
                diagnostics.push(PendingDiagnostic::warning(
                    "metadata.value-dropped",
                    format!("`metadata.{key}` is not a string; dropped"),
                ));
            }
        }
    }
    metadata
}

fn parse_extension_fields(
    mapping: &BTreeMap<String, YamlValue>,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> ClaudeExt {
    let mut ext = ClaudeExt {
        user_invocable: true,
        ..ClaudeExt::default()
    };

    ext.when_to_use = string_field(mapping, "when_to_use", diagnostics);
    ext.model = string_field(mapping, "model", diagnostics);
    ext.effort = string_field(mapping, "effort", diagnostics);
    ext.argument_hint = string_field(mapping, "argument-hint", diagnostics);

    if let Some(value) = mapping.get("disable-model-invocation") {
        match lenient_bool(value) {
            Some(flag) => ext.disable_model_invocation = flag,
            None => diagnostics.push(PendingDiagnostic::warning(
                "ext.bool",
                "`disable-model-invocation` is not a boolean; ignored",
            )),
        }
    }
    if let Some(value) = mapping.get("user-invocable") {
        match lenient_bool(value) {
            Some(flag) => ext.user_invocable = flag,
            None => diagnostics.push(PendingDiagnostic::warning(
                "ext.bool",
                "`user-invocable` is not a boolean; ignored",
            )),
        }
    }

    ext.disallowed_tools = tool_list(
        mapping.get("disallowed-tools"),
        "disallowed-tools",
        diagnostics,
    );
    ext.paths = tool_list(mapping.get("paths"), "paths", diagnostics);
    ext.arguments = tool_list(mapping.get("arguments"), "arguments", diagnostics);

    match mapping.get("context").and_then(YamlValue::as_str) {
        Some("fork") => {
            let mut fork = ForkHint {
                agent: string_field(mapping, "agent", diagnostics),
                ..ForkHint::default()
            };
            if let Some(value) = mapping.get("background") {
                match lenient_bool(value) {
                    Some(flag) => fork.background = flag,
                    None => diagnostics.push(PendingDiagnostic::warning(
                        "ext.bool",
                        "`background` is not a boolean; ignored",
                    )),
                }
            }
            ext.fork = Some(fork);
        }
        Some(other) => diagnostics.push(PendingDiagnostic::warning(
            "ext.context",
            format!("`context: {other}` is not recognized; ignored"),
        )),
        None => {}
    }

    // Declared-but-unsupported extensions: decline loudly instead of
    // pretending (RFC-121 compatibility matrix).
    for field in ["hooks", "shell"] {
        if mapping.contains_key(field) {
            diagnostics.push(PendingDiagnostic::info(
                "ext.unsupported",
                format!("`{field}` is a Claude Code extension agenkitty does not execute; ignored"),
            ));
        }
    }

    ext
}

fn string_field(
    mapping: &BTreeMap<String, YamlValue>,
    key: &'static str,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> Option<String> {
    match mapping.get(key) {
        None => None,
        Some(YamlValue::String(text)) => Some(text.clone()),
        Some(_) => {
            diagnostics.push(PendingDiagnostic::warning(
                "field.type",
                format!("`{key}` is not a string; ignored"),
            ));
            None
        }
    }
}

/// Normalize a space/comma-separated string or a YAML list of strings
/// (`allowed-tools`, `disallowed-tools`, `paths`, `arguments`).
fn tool_list(
    value: Option<&YamlValue>,
    key: &'static str,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> Vec<String> {
    match value {
        None => Vec::new(),
        Some(YamlValue::String(text)) => text
            .split([' ', ','])
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect(),
        Some(YamlValue::Sequence(sequence)) => {
            let mut entries = Vec::new();
            for item in sequence {
                match item.as_str() {
                    Some(entry) if !entry.trim().is_empty() => {
                        entries.push(entry.trim().to_string());
                    }
                    _ => diagnostics.push(PendingDiagnostic::warning(
                        "field.type",
                        format!("`{key}` list entries must be strings; entry dropped"),
                    )),
                }
            }
            entries
        }
        Some(_) => {
            diagnostics.push(PendingDiagnostic::warning(
                "field.type",
                format!("`{key}` must be a string or a list of strings; ignored"),
            ));
            Vec::new()
        }
    }
}

/// Claude Code boolean leniency: `yes`/`no`/`on`/`off`/`1`/`0` in any case,
/// plus real YAML booleans and 0/1 numbers.
fn lenient_bool(value: &YamlValue) -> Option<bool> {
    match value {
        YamlValue::Bool(flag) => Some(*flag),
        YamlValue::Number(number) => match number.as_u64() {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        },
        YamlValue::String(text) => match text.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Some(true),
            "false" | "no" | "off" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn yaml_key_to_string(key: &YamlValue) -> String {
    match key {
        YamlValue::String(text) => text.clone(),
        other => serde_norway::to_string(other)
            .map(|text| text.trim_end_matches('\n').to_string())
            .unwrap_or_else(|_| "?".to_string()),
    }
}

fn yaml_to_json(value: &YamlValue) -> JsonValue {
    match value {
        YamlValue::Null => JsonValue::Null,
        YamlValue::Bool(flag) => JsonValue::Bool(*flag),
        YamlValue::Number(number) => {
            if let Some(int) = number.as_i64() {
                JsonValue::Number(int.into())
            } else if let Some(int) = number.as_u64() {
                JsonValue::Number(int.into())
            } else {
                serde_json::Number::from_f64(number.as_f64().unwrap_or(0.0))
                    .map(JsonValue::Number)
                    .unwrap_or(JsonValue::Null)
            }
        }
        YamlValue::String(text) => JsonValue::String(text.clone()),
        YamlValue::Sequence(sequence) => {
            JsonValue::Array(sequence.iter().map(yaml_to_json).collect())
        }
        YamlValue::Mapping(mapping) => JsonValue::Object(
            mapping
                .iter()
                .map(|(key, value)| (yaml_key_to_string(key), yaml_to_json(value)))
                .collect(),
        ),
        YamlValue::Tagged(tagged) => yaml_to_json(&tagged.value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(dir: &str, src: &str) -> ParsedSkillFile {
        parse_skill_file(dir, src)
    }

    fn errors(parsed: &ParsedSkillFile) -> Vec<&'static str> {
        parsed
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.rule)
            .collect()
    }

    #[test]
    fn minimal_valid_skill_parses() {
        let parsed = parse(
            "pdf-processing",
            "---\nname: pdf-processing\ndescription: Extract PDF text. Use for PDFs.\n---\nBody here.\n",
        );
        let meta = parsed.meta.as_ref().expect("valid skill");
        assert_eq!(meta.name, "pdf-processing");
        assert_eq!(parsed.body, "Body here.\n");
        assert!(errors(&parsed).is_empty());
    }

    #[test]
    fn missing_frontmatter_is_an_error() {
        let parsed = parse("x", "just markdown, no frontmatter\n");
        assert!(parsed.meta.is_none());
        assert_eq!(errors(&parsed), vec!["frontmatter.missing"]);
    }

    #[test]
    fn unterminated_frontmatter_is_an_error() {
        let parsed = parse("x", "---\nname: x\ndescription: y\n");
        assert!(parsed.meta.is_none());
        assert_eq!(errors(&parsed), vec!["frontmatter.missing"]);
    }

    #[test]
    fn every_name_rule_fires() {
        let cases: [(&str, &str, &str); 5] = [
            ("PDF-Processing", "pdf-processing", "name.charset"),
            ("-pdf", "-pdf", "name.hyphen-edge"),
            ("pdf-", "pdf-", "name.hyphen-edge"),
            ("pdf--processing", "pdf--processing", "name.hyphen-run"),
            ("other-name", "the-dir", "name.dir-mismatch"),
        ];
        for (name, dir, rule) in cases {
            let src = format!("---\nname: {name}\ndescription: d\n---\n");
            let parsed = parse(dir, &src);
            assert!(parsed.meta.is_none(), "{name} should be excluded");
            assert!(errors(&parsed).contains(&rule), "{name} → {rule}");
        }
        let long = "a".repeat(65);
        let src = format!("---\nname: {long}\ndescription: d\n---\n");
        let parsed = parse(&long, &src);
        assert!(errors(&parsed).contains(&"name.length"));
    }

    #[test]
    fn description_bounds_enforced() {
        let parsed = parse("x", "---\nname: x\ndescription: \"  \"\n---\n");
        assert!(errors(&parsed).contains(&"description.empty"));

        let long = "d".repeat(1025);
        let parsed = parse("x", &format!("---\nname: x\ndescription: {long}\n---\n"));
        assert!(errors(&parsed).contains(&"description.length"));
    }

    #[test]
    fn compatibility_bound_enforced() {
        let long = "c".repeat(501);
        let parsed = parse(
            "x",
            &format!("---\nname: x\ndescription: d\ncompatibility: {long}\n---\n"),
        );
        assert!(errors(&parsed).contains(&"compatibility.length"));
    }

    #[test]
    fn metadata_coerces_scalars_and_drops_nested() {
        let parsed = parse(
            "x",
            "---\nname: x\ndescription: d\nmetadata:\n  author: org\n  version: 1.0\n  nested:\n    a: b\n---\n",
        );
        let meta = parsed.meta.expect("loadable");
        assert_eq!(meta.metadata.get("author").map(String::as_str), Some("org"));
        assert!(meta.metadata.contains_key("version"));
        assert!(!meta.metadata.contains_key("nested"));
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|d| d.rule == "metadata.value-dropped")
        );
    }

    #[test]
    fn allowed_tools_accepts_all_three_shapes() {
        for tools in [
            "allowed-tools: fs.read fs.list",
            "allowed-tools: fs.read, fs.list",
            "allowed-tools:\n  - fs.read\n  - fs.list",
        ] {
            let parsed = parse(
                "x",
                &format!("---\nname: x\ndescription: d\n{tools}\n---\n"),
            );
            let meta = parsed.meta.expect("loadable");
            assert_eq!(meta.allowed_tools, vec!["fs.read", "fs.list"], "{tools}");
        }
    }

    #[test]
    fn extension_fields_parse_without_acting() {
        let parsed = parse(
            "x",
            "---\nname: x\ndescription: d\nwhen_to_use: On demand\ndisable-model-invocation: yes\nuser-invocable: 'off'\ncontext: fork\nagent: reviewer\nbackground: 'no'\nhooks:\n  - nope\n---\n",
        );
        assert!(parsed.meta.is_some());
        assert_eq!(parsed.ext.when_to_use.as_deref(), Some("On demand"));
        assert!(parsed.ext.disable_model_invocation);
        assert!(!parsed.ext.user_invocable);
        let fork = parsed.ext.fork.as_ref().expect("fork hint");
        assert_eq!(fork.agent.as_deref(), Some("reviewer"));
        assert!(!fork.background);
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|d| d.rule == "ext.unsupported" && d.message.contains("hooks"))
        );
    }

    #[test]
    fn unknown_fields_are_preserved_silently() {
        let parsed = parse(
            "x",
            "---\nname: x\ndescription: d\nx-custom: value\n---\nbody",
        );
        assert!(parsed.meta.is_some());
        assert_eq!(
            parsed.raw.get("x-custom"),
            Some(&JsonValue::String("value".to_string()))
        );
        assert!(
            !parsed
                .diagnostics
                .iter()
                .any(|d| d.message.contains("x-custom"))
        );
    }
}
