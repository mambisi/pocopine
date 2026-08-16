//! Shared plumbing for the skill tool family: the caller context, the
//! [`SkillRuntime`] (catalog + activation state + `context_token` handshake,
//! mirroring `MemoryRuntime`), and the attenuation-only visibility model
//! (RFC-121 § Subagents).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use agenkitty_core::config::SkillsConfigSection;
use agenkitty_skills::{LoadedSkill, SkillCatalog, SkillError, SkillLimits, SkillLoader};
use pocopine_agenkit::server::session::ThreadId;
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use serde_json::{Map, Value};

/// Caller identity resolved from a runtime-injected `context_token`.
/// Visibility attenuation rides here so in-process subagents sharing the
/// registered tools still get their own narrowed view.
#[derive(Clone, Debug)]
pub struct CurrentSkillContext {
    pub agent_id: String,
    pub thread_id: Option<String>,
    /// `None` = the runtime's own view. `Some` may only narrow it further —
    /// the effective view is always the intersection.
    pub visible: Option<Arc<BTreeSet<String>>>,
}

impl CurrentSkillContext {
    /// Activation state is per session: keyed by thread when known, else by
    /// agent (mirrors the memory family's session namespace).
    pub fn activation_key(&self) -> String {
        self.thread_id
            .as_deref()
            .filter(|thread| !thread.is_empty())
            .unwrap_or(self.agent_id.as_str())
            .to_string()
    }
}

/// Holds the discovered [`SkillCatalog`], the runtime-level visible set, the
/// per-session activation records, and the `context_token` → context map.
pub struct SkillRuntime {
    catalog: RwLock<Arc<SkillCatalog>>,
    /// The runtime-level view. `None` = every catalog skill. A forked child
    /// runtime carries the narrowed set (RFC-118 attenuation recipe).
    visible: Option<BTreeSet<String>>,
    contexts: Mutex<HashMap<String, CurrentSkillContext>>,
    token_seq: AtomicU64,
    activations: Mutex<HashMap<String, HashSet<String>>>,
}

impl SkillRuntime {
    pub fn new(catalog: SkillCatalog) -> Self {
        Self {
            catalog: RwLock::new(Arc::new(catalog)),
            visible: None,
            contexts: Mutex::new(HashMap::new()),
            token_seq: AtomicU64::new(0),
            activations: Mutex::new(HashMap::new()),
        }
    }

    /// No skills: every lookup is `not_found`, the prompt part is `None`.
    pub fn empty() -> Self {
        Self::new(SkillCatalog::default())
    }

    /// Discover per the project's `[skills]` config. Disabled → empty.
    pub fn from_config(project_root: &Path, section: &SkillsConfigSection) -> Self {
        if !section.enabled {
            return Self::empty();
        }
        let catalog = SkillLoader::new(resolve_skill_roots(project_root, section))
            .with_limits(limits_from_config(section))
            .discover();
        // Observability per RFC-069: counts only — never names, descriptions,
        // or paths beyond the project root already in scope.
        tracing::info!(
            target: "pocopine.log",
            skills = catalog.len(),
            shadowed = catalog.shadowed().len(),
            diagnostics = catalog.diagnostics().len(),
            "skill catalog discovered"
        );
        Self::new(catalog)
    }

    /// The current catalog snapshot. Tools clone the `Arc` out and work on a
    /// consistent view even across a concurrent [`refresh`](Self::refresh).
    pub fn catalog(&self) -> Arc<SkillCatalog> {
        self.catalog
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Replace the catalog (host rescan seam). Activations survive: an
    /// activated skill that vanished simply stops resolving.
    pub fn refresh(&self, catalog: SkillCatalog) {
        if let Ok(mut guard) = self.catalog.write() {
            *guard = Arc::new(catalog);
        }
    }

    /// Whether the runtime-level view admits `name`.
    fn runtime_visible(&self, name: &str) -> bool {
        self.visible
            .as_ref()
            .is_none_or(|visible| visible.contains(name))
    }

    /// Resolve a skill through both attenuation layers. Out-of-view and
    /// nonexistent names are indistinguishable to the caller (RFC-121 S7).
    pub fn visible_skill<'c>(
        &self,
        catalog: &'c SkillCatalog,
        context: &CurrentSkillContext,
        name: &str,
    ) -> Option<&'c LoadedSkill> {
        if !self.runtime_visible(name) {
            return None;
        }
        if let Some(visible) = &context.visible
            && !visible.contains(name)
        {
            return None;
        }
        catalog.get(name)
    }

    /// The `## Skills` system-prompt part (level 1), rendered through the
    /// runtime's view so prompt and enforcement cannot disagree. `None` when
    /// no skill is advertisable. The budget always comes from the *current*
    /// catalog's limits, so a `refresh` with new budgets takes effect here.
    pub fn system_prompt_part(&self) -> Option<String> {
        self.system_prompt_part_for_view(None)
    }

    /// [`system_prompt_part`](Self::system_prompt_part) for a caller that
    /// will narrow via `CurrentSkillContext::visible` — the same intersection
    /// the tools enforce, so a context-attenuated subagent's prompt never
    /// advertises a skill its `skill.use` would refuse.
    pub fn system_prompt_part_for_view(&self, narrow: Option<&BTreeSet<String>>) -> Option<String> {
        let catalog = self.catalog();
        let part = catalog.render_prompt_part_where(catalog.limits().index_byte_budget, |skill| {
            self.runtime_visible(&skill.meta.name)
                && narrow.is_none_or(|visible| visible.contains(&skill.meta.name))
        });
        if part.is_empty() { None } else { Some(part) }
    }

    /// Record that `name` was activated for this session key.
    pub fn activate(&self, activation_key: &str, name: &str) {
        if let Ok(mut activations) = self.activations.lock() {
            activations
                .entry(activation_key.to_string())
                .or_default()
                .insert(name.to_string());
        }
    }

    /// Whether `skill.read` is unlocked for this session key.
    pub fn is_activated(&self, activation_key: &str, name: &str) -> bool {
        self.activations
            .lock()
            .map(|activations| {
                activations
                    .get(activation_key)
                    .is_some_and(|names| names.contains(name))
            })
            .unwrap_or(false)
    }

    /// Attenuation-only fork for hosts that give a subagent its own runtime
    /// (RFC-118 recipe): the child shares the catalog snapshot, starts with
    /// fresh activations, and sees `narrow ∩ parent`. Widening — naming a
    /// skill the parent cannot see — is a loud error, never a silent clamp.
    pub fn fork(&self, narrow: Option<&BTreeSet<String>>) -> AgenkitResult<SkillRuntime> {
        let catalog = self.catalog();
        let child_visible = match narrow {
            None => self.visible.clone(),
            Some(requested) => {
                for name in requested {
                    let parent_has = self.runtime_visible(name) && catalog.get(name).is_some();
                    if !parent_has {
                        return Err(AgenkitError::validation(format!(
                            "skill view may only narrow: `{name}` is not visible to the parent"
                        )));
                    }
                }
                Some(requested.clone())
            }
        };
        Ok(SkillRuntime {
            catalog: RwLock::new(catalog),
            visible: child_visible,
            contexts: Mutex::new(HashMap::new()),
            token_seq: AtomicU64::new(0),
            activations: Mutex::new(HashMap::new()),
        })
    }

    /// Inject a fresh `context_token` into the tool arguments so the tool can
    /// recover the caller's [`CurrentSkillContext`].
    pub fn inject_context_args(
        &self,
        args: &Value,
        context: CurrentSkillContext,
    ) -> Result<Value, String> {
        let mut object = match args {
            Value::Null => Map::new(),
            Value::Object(object) => object.clone(),
            _ => return Err("skill tool arguments must be a JSON object".to_string()),
        };
        let token = self.issue_context_token(context)?;
        object.insert("context_token".to_string(), Value::String(token));
        Ok(Value::Object(object))
    }

    /// Consume the context behind a token. A token is single-use.
    pub fn take_context(&self, token: &str) -> AgenkitResult<CurrentSkillContext> {
        let mut contexts = self.contexts.lock().map_err(lock_err)?;
        contexts.remove(token).ok_or_else(|| {
            AgenkitError::validation("skill tool called without an active skill context")
        })
    }

    fn issue_context_token(&self, context: CurrentSkillContext) -> Result<String, String> {
        let random = ThreadId::mint().map_err(|err| err.to_string())?;
        let seq = self.token_seq.fetch_add(1, Ordering::Relaxed);
        let token = format!("{}-{seq}", random.as_str());
        let mut contexts = self.contexts.lock().map_err(|err| err.to_string())?;
        contexts.insert(token.clone(), context);
        Ok(token)
    }
}

/// Resolve the config's ordered roots against the project root.
pub fn resolve_skill_roots(project_root: &Path, section: &SkillsConfigSection) -> Vec<PathBuf> {
    section
        .roots
        .iter()
        .map(|root| {
            if root.is_absolute() {
                root.clone()
            } else {
                project_root.join(root)
            }
        })
        .collect()
}

/// The loader limits carried by the `[skills]` config section.
pub fn limits_from_config(section: &SkillsConfigSection) -> SkillLimits {
    SkillLimits {
        entry_byte_cap: section.entry_byte_cap,
        index_byte_budget: section.index_byte_budget,
        body_byte_limit: section.body_byte_limit,
        ..SkillLimits::default()
    }
}

/// Map a loader error onto the Agenkit error taxonomy, kind for kind.
pub(crate) fn map_skill_error(err: SkillError) -> AgenkitError {
    let message = err.to_string();
    match err.kind() {
        "not_found" => AgenkitError::not_found(message),
        "tool_policy" => AgenkitError::tool_policy(message),
        "validation" => AgenkitError::validation(message),
        _ => AgenkitError::internal(message),
    }
}

fn lock_err<T>(_: std::sync::PoisonError<T>) -> AgenkitError {
    AgenkitError::internal("skill runtime lock poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_key_prefers_thread_then_agent() {
        let with_thread = CurrentSkillContext {
            agent_id: "agent".to_string(),
            thread_id: Some("thread-1".to_string()),
            visible: None,
        };
        assert_eq!(with_thread.activation_key(), "thread-1");
        let without = CurrentSkillContext {
            agent_id: "agent".to_string(),
            thread_id: None,
            visible: None,
        };
        assert_eq!(without.activation_key(), "agent");
    }

    #[test]
    fn context_tokens_are_single_use() {
        let runtime = SkillRuntime::empty();
        let args = runtime
            .inject_context_args(
                &serde_json::json!({ "name": "x" }),
                CurrentSkillContext {
                    agent_id: "agent".to_string(),
                    thread_id: None,
                    visible: None,
                },
            )
            .unwrap();
        let token = args["context_token"].as_str().unwrap().to_string();
        assert!(runtime.take_context(&token).is_ok());
        assert!(runtime.take_context(&token).is_err());
    }

    #[test]
    fn empty_runtime_has_no_prompt_part() {
        assert_eq!(SkillRuntime::empty().system_prompt_part(), None);
    }

    #[test]
    fn config_defaults_agree_with_loader_defaults() {
        // The [skills] section defaults live in agenkitty-core and cannot
        // import SkillLimits (dependency direction); this guard keeps the
        // duplicated numbers from drifting.
        let from_config = limits_from_config(&SkillsConfigSection::default());
        let library = SkillLimits::default();
        assert_eq!(from_config.entry_byte_cap, library.entry_byte_cap);
        assert_eq!(from_config.index_byte_budget, library.index_byte_budget);
        assert_eq!(from_config.body_byte_limit, library.body_byte_limit);
    }
}
