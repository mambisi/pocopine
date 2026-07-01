//! Capability-scoped admission for imported MCP tools (D5) — the headline
//! differentiator over name-only allow/deny lists.
//!
//! Every remote `tools/call` resolves to an [`AdmissionDecision`] over the
//! `(server, tool, capability)` tuple **before** dispatch:
//!
//! - An **allowlisted** tool is `Allow` (an explicit operator decision).
//! - An **un-allowlisted tool on an untrusted server** is `Deny` (the capability
//!   gate; the default for newly-discovered servers).
//! - A **trusted** server's un-allowlisted tools fall back to its configured
//!   `default_mode`, which untrusted **annotation hints** may only *tighten*
//!   (e.g. a `destructiveHint` can turn `Allow` into `Ask`).
//!
//! Tool annotations (`readOnlyHint`/`destructiveHint`/…) are **untrusted hints**
//! (D9): they feed the default-mode path only and can never loosen a decision or
//! override a `Deny` — a poisoned `destructiveHint: false` cannot promote a
//! denied tool to `Allow`.

use rmcp::model::ToolAnnotations;
use serde_json::{Map, Value};
use url::Url;

use crate::policy::ToolMode;

use super::config::{McpServerConfig, McpTransportConfig};

/// The admission outcome for one `(server, tool)` pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionDecision {
    /// Dispatch is permitted.
    Allow,
    /// Dispatch requires host approval; the payload carries the model-safe,
    /// **pinned** description + params the approval UI must render (T1).
    Ask(AskPrompt),
    /// Dispatch is refused; `reason` is a model-safe explanation.
    Deny { reason: String },
}

/// Where an approval's tool runs — the concrete connection target a host UI must
/// show an operator alongside the tool so they can judge the request (T1). Carries
/// **no secret material**: stdio env is resolved late from secret handles and is
/// never part of this, and the HTTP variant is the endpoint **origin only**
/// (scheme://host[:port]), never the full URL path/query (which can carry tokens).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalTransport {
    /// A local stdio server: the exact child the host would spawn.
    Stdio {
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
    },
    /// A remote HTTP server, identified by its endpoint origin only.
    Http { origin: String },
}

impl ApprovalTransport {
    /// A one-line summary for an audit/event detail.
    ///
    /// The stdio variant surfaces the **command path + arg COUNT only** — never
    /// the raw argv (RM1). Env secrets are already late-resolved out of the
    /// payload, but an operator could accidentally put a credential in a plain
    /// argument; the audit detail must not carry it (the observer's substring
    /// redaction is a heuristic that may not match a secret-shaped arg). The full
    /// `args` remain in the [`AskPrompt`] DATA (this enum's own fields) for a host
    /// UI to render the complete prompt T1 demands — only this summary string,
    /// which lands in an audit row, drops them. `cwd` is an operator-configured
    /// path (not argv), kept as useful, low-risk context.
    pub fn summary(&self) -> String {
        match self {
            ApprovalTransport::Stdio { command, args, cwd } => {
                let mut s = format!("stdio: {command}");
                if !args.is_empty() {
                    s.push_str(&format!(" ({} arg(s))", args.len()));
                }
                if let Some(cwd) = cwd {
                    s.push_str(&format!(" (cwd: {cwd})"));
                }
                s
            }
            ApprovalTransport::Http { origin } => format!("http: {origin}"),
        }
    }
}

/// The data an `Ask` approval surfaces. Carries the **pinned** description and
/// input schema (never a live re-fetch), so what the operator approves is exactly
/// what the model will see (T1/T2), **plus** the concrete connection target
/// (stdio command/args/cwd or the HTTP origin) so a host UI can render the full
/// "tool X on server Y, which runs `<command>`, wants to do Z" prompt T1 demands.
///
/// The `description` is the pinned, sanitized description verbatim — i.e. exactly
/// the (16 KiB-bounded) text the model sees; it is **not** further truncated here,
/// so the operator approves precisely the model-visible text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskPrompt {
    /// The owning server name.
    pub server: String,
    /// The server-local tool name.
    pub tool: String,
    /// The pinned, sanitized description (the full model-visible text).
    pub description: Option<String>,
    /// The pinned input schema (the exact params the call will accept).
    pub params_schema: Map<String, Value>,
    /// The concrete connection target (stdio command/args/cwd or HTTP origin).
    pub transport: ApprovalTransport,
}

impl AskPrompt {
    /// A one-line, secret-free summary of this request for an audit/event detail.
    pub fn summary(&self) -> String {
        format!(
            "{} / {} — {}",
            self.server,
            self.tool,
            self.transport.summary()
        )
    }
}

/// The concrete connection target for a server, for an [`AskPrompt`] (T1). The
/// HTTP origin is derived as `scheme://host[:port]` (path/query dropped); a
/// URL that fails to parse falls back to the raw (operator-configured) string.
fn approval_transport(config: &McpServerConfig) -> ApprovalTransport {
    match &config.transport {
        McpTransportConfig::Stdio {
            command, args, cwd, ..
        } => ApprovalTransport::Stdio {
            command: command.clone(),
            args: args.clone(),
            cwd: cwd.clone(),
        },
        McpTransportConfig::Http { url, .. } => ApprovalTransport::Http {
            origin: http_origin(url),
        },
    }
}

/// `scheme://host[:port]` for an endpoint URL — never the path/query.
fn http_origin(url: &str) -> String {
    match Url::parse(url) {
        Ok(parsed) => {
            let scheme = parsed.scheme();
            let host = parsed.host_str().unwrap_or("");
            match parsed.port() {
                Some(port) => format!("{scheme}://{host}:{port}"),
                None => format!("{scheme}://{host}"),
            }
        }
        Err(_) => url.to_string(),
    }
}

/// Build the T1-complete approval payload for a `(server, tool)` pair from its
/// pinned `description` + `params_schema` and the server's connection target.
/// Shared by [`admit`]'s `Ask` arm and the imported-tool adapter's fail-closed
/// `Ask` branch so both surface the identical payload.
pub fn build_ask_prompt(
    config: &McpServerConfig,
    tool_name: &str,
    description: Option<&str>,
    params_schema: &Map<String, Value>,
) -> AskPrompt {
    AskPrompt {
        server: config.name.clone(),
        tool: tool_name.to_string(),
        description: description.map(str::to_string),
        params_schema: params_schema.clone(),
        transport: approval_transport(config),
    }
}

/// Resolve the base policy mode for a `(server, tool)` pair, honoring the
/// allowlist, the trust flag, the configured default mode, and untrusted
/// annotation hints.
///
/// Annotation hints can only ever make the result *stricter*; they are applied
/// solely on the trusted-default-mode path and never reached for an explicit
/// allowlist entry or for the untrusted-`Deny` gate.
pub fn resolve_mode(
    config: &McpServerConfig,
    tool_name: &str,
    annotations: Option<&ToolAnnotations>,
) -> ToolMode {
    // An explicit allowlist entry is an operator decision: trust it as-is.
    if config.allowed_tools.iter().any(|t| t == tool_name) {
        return ToolMode::Allow;
    }
    // Un-allowlisted tool on an untrusted server → Deny (the capability gate).
    // Reached before annotations, so no hint can promote it.
    if !config.trusted {
        return ToolMode::Deny;
    }
    // Trusted server: the configured default, tightened by untrusted hints.
    tighten_with_hints(config.default_mode, annotations)
}

/// Apply annotation hints, which may only tighten the mode (never loosen it).
/// A tool the server itself flags destructive escalates `Allow` → `Ask`.
fn tighten_with_hints(mode: ToolMode, annotations: Option<&ToolAnnotations>) -> ToolMode {
    let destructive = annotations
        .and_then(|a| a.destructive_hint)
        .unwrap_or(false);
    if destructive && mode == ToolMode::Allow {
        return ToolMode::Ask;
    }
    mode
}

/// Resolve the full [`AdmissionDecision`] for a `(server, tool)` pair, building
/// the `Ask` approval payload from the pinned description + schema.
pub fn admit(
    config: &McpServerConfig,
    tool_name: &str,
    annotations: Option<&ToolAnnotations>,
    description: Option<&str>,
    params_schema: &Map<String, Value>,
) -> AdmissionDecision {
    match resolve_mode(config, tool_name, annotations) {
        ToolMode::Allow => AdmissionDecision::Allow,
        ToolMode::Ask => AdmissionDecision::Ask(build_ask_prompt(
            config,
            tool_name,
            description,
            params_schema,
        )),
        ToolMode::Deny => AdmissionDecision::Deny {
            reason: format!(
                "tool `{}` on server `{}` is denied (un-allowlisted on an untrusted server)",
                tool_name, config.name
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mcp::config::McpServerConfig;

    fn annotations(read_only: Option<bool>, destructive: Option<bool>) -> ToolAnnotations {
        // `ToolAnnotations` is `#[non_exhaustive]`; build via its constructor.
        ToolAnnotations::from_raw(None, read_only, destructive, None, None)
    }

    #[test]
    fn untrusted_unallowlisted_tool_is_denied() {
        let config = McpServerConfig::stdio("s", "c"); // untrusted, no allowlist
        assert_eq!(resolve_mode(&config, "anything", None), ToolMode::Deny);
        assert!(matches!(
            admit(&config, "anything", None, Some("desc"), &Map::new()),
            AdmissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn allowlisted_tool_is_allowed_even_when_untrusted() {
        let mut config = McpServerConfig::stdio("s", "c");
        config.allowed_tools = vec!["create_issue".to_string()];
        assert_eq!(resolve_mode(&config, "create_issue", None), ToolMode::Allow);
        assert_eq!(
            admit(&config, "create_issue", None, None, &Map::new()),
            AdmissionDecision::Allow
        );
    }

    #[test]
    fn trusted_server_falls_back_to_default_mode() {
        let mut config = McpServerConfig::stdio("s", "c");
        config.trusted = true;
        config.default_mode = ToolMode::Ask;
        assert_eq!(resolve_mode(&config, "other", None), ToolMode::Ask);
        config.default_mode = ToolMode::Allow;
        assert_eq!(resolve_mode(&config, "other", None), ToolMode::Allow);
    }

    #[test]
    fn destructive_hint_tightens_allow_to_ask_on_trusted_server() {
        let mut config = McpServerConfig::stdio("s", "c");
        config.trusted = true;
        config.default_mode = ToolMode::Allow;
        // A trusted server's default-Allow tool that flags itself destructive is
        // tightened to Ask (the hint may only make things stricter).
        assert_eq!(
            resolve_mode(&config, "rm", Some(&annotations(None, Some(true)))),
            ToolMode::Ask
        );
    }

    #[test]
    fn destructive_hint_cannot_promote_deny_to_allow() {
        // Untrusted server → Deny. A poisoned `destructiveHint: false` (claiming
        // the tool is harmless) must NOT promote it to Allow.
        let config = McpServerConfig::stdio("s", "c");
        let benign_claim = annotations(Some(true), Some(false));
        assert_eq!(
            resolve_mode(&config, "exfiltrate", Some(&benign_claim)),
            ToolMode::Deny
        );
        assert!(matches!(
            admit(
                &config,
                "exfiltrate",
                Some(&benign_claim),
                None,
                &Map::new()
            ),
            AdmissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn ask_payload_carries_server_pinned_description_params_and_stdio_command() {
        // M4: the approval payload must carry the exact spawn command/args/cwd
        // (stdio), the full pinned description, and the full params schema so a
        // host UI can satisfy T1.
        let mut config = McpServerConfig::stdio("github", "github-mcp");
        config.trusted = true;
        config.default_mode = ToolMode::Ask;
        if let McpTransportConfig::Stdio { args, cwd, .. } = &mut config.transport {
            *args = vec!["--stdio".to_string()];
            *cwd = Some("repo".to_string());
        }
        let schema: Map<String, Value> = serde_json::from_value(
            serde_json::json!({ "type": "object", "properties": { "title": { "type": "string" } } }),
        )
        .unwrap();
        let decision = admit(
            &config,
            "create_issue",
            None,
            Some("Open an issue"),
            &schema,
        );
        let AdmissionDecision::Ask(prompt) = decision else {
            panic!("expected Ask");
        };
        assert_eq!(prompt.server, "github");
        assert_eq!(prompt.tool, "create_issue");
        assert_eq!(prompt.description.as_deref(), Some("Open an issue"));
        assert_eq!(
            prompt.params_schema, schema,
            "full params schema is carried"
        );
        assert_eq!(
            prompt.transport,
            ApprovalTransport::Stdio {
                command: "github-mcp".to_string(),
                args: vec!["--stdio".to_string()],
                cwd: Some("repo".to_string()),
            },
            "the exact spawn command/args/cwd are carried"
        );
        // The audit summary surfaces the command path (and an arg COUNT) but NOT
        // the raw argv (RM1) — the full args live in the DATA above, not the
        // summary string that lands in an audit row.
        let summary = prompt.summary();
        assert!(
            summary.contains("github-mcp"),
            "command path in summary: {summary}"
        );
        assert!(
            !summary.contains("--stdio"),
            "raw args must not appear in the audit summary (RM1): {summary}"
        );
        assert!(
            summary.contains("1 arg(s)"),
            "arg count is surfaced: {summary}"
        );
    }

    #[test]
    fn summary_omits_raw_args_so_a_secret_in_argv_cannot_leak() {
        // RM1: the audit summary must not carry raw argv. A credential accidentally
        // placed in a plain argument would otherwise reach the audit detail unless
        // the observer's substring heuristic happened to match it. The full args
        // remain in the AskPrompt DATA (transport) for a host UI; only the summary
        // string drops them.
        let mut config = McpServerConfig::stdio("srv", "the-binary");
        let secret_arg = "sk-deadbeefcafe1234567890";
        if let McpTransportConfig::Stdio { args, .. } = &mut config.transport {
            *args = vec!["--token".to_string(), secret_arg.to_string()];
        }
        config.trusted = true;
        config.default_mode = ToolMode::Ask;
        let AdmissionDecision::Ask(prompt) = admit(&config, "t", None, Some("d"), &Map::new())
        else {
            panic!("expected Ask");
        };
        let summary = prompt.summary();
        assert!(
            summary.contains("the-binary"),
            "command path kept: {summary}"
        );
        assert!(
            !summary.contains(secret_arg),
            "a secret-shaped arg must not appear in the audit summary: {summary}"
        );
        // …but the full args are still carried as DATA for a host UI (T1).
        assert_eq!(
            prompt.transport,
            ApprovalTransport::Stdio {
                command: "the-binary".to_string(),
                args: vec!["--token".to_string(), secret_arg.to_string()],
                cwd: None,
            }
        );
    }

    #[test]
    fn ask_payload_carries_http_origin_only() {
        // For an HTTP server the payload carries the endpoint ORIGIN only — never
        // the full URL path/query (which can carry tokens).
        let mut config = McpServerConfig::stdio("remote", "unused");
        config.transport = McpTransportConfig::Http {
            url: "https://mcp.example.com:8443/mcp?token=abc".to_string(),
            headers: std::collections::BTreeMap::new(),
        };
        config.trusted = true;
        config.default_mode = ToolMode::Ask;
        let schema: Map<String, Value> =
            serde_json::from_value(serde_json::json!({ "type": "object" })).unwrap();
        let AdmissionDecision::Ask(prompt) =
            admit(&config, "search", None, Some("Search"), &schema)
        else {
            panic!("expected Ask");
        };
        assert_eq!(
            prompt.transport,
            ApprovalTransport::Http {
                origin: "https://mcp.example.com:8443".to_string(),
            },
            "only scheme://host:port — never the path/query"
        );
        let summary = prompt.summary();
        assert!(summary.contains("https://mcp.example.com:8443"));
        assert!(!summary.contains("token=abc"), "path/query must not leak");
    }
}
