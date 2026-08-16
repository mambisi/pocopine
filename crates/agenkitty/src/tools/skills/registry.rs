//! Registration helpers for the skill tool family.
//!
//! Both tools are registered against one [`SkillRuntime`]; the runtime's
//! catalog decides what they can serve. An empty runtime keeps the ids
//! resolvable while every lookup answers `not_found`.

use std::sync::Arc;

use pocopine_agenkit::server::AgenkitBuilder;

use super::common::SkillRuntime;
use super::read::{SKILL_READ_TOOL_ID, SkillReadTool};
use super::use_skill::{SKILL_USE_TOOL_ID, SkillUseTool};

/// Register both model-facing skill tools against one runtime.
pub fn register_skill_tools(builder: AgenkitBuilder, runtime: Arc<SkillRuntime>) -> AgenkitBuilder {
    builder
        .tool(SkillUseTool::new(runtime.clone()))
        .tool(SkillReadTool::new(runtime))
}

/// Every skill tool id, in a stable order.
pub fn known_skill_tool_ids() -> [&'static str; 2] {
    [SKILL_USE_TOOL_ID, SKILL_READ_TOOL_ID]
}

/// Validate and de-duplicate a caller-supplied list of skill tool ids.
pub fn resolve_skill_tool_ids(raw: &[String]) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    for value in raw {
        for id in value.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            if !known_skill_tool_ids().contains(&id) {
                return Err(format!("unknown skill tool `{id}`"));
            }
            if !ids.iter().any(|existing| existing == id) {
                ids.push(id.to_string());
            }
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_skill_tool_ids_validates_and_dedupes() {
        assert!(resolve_skill_tool_ids(&[]).unwrap().is_empty());
        assert_eq!(
            resolve_skill_tool_ids(&["skill.use,skill.use".to_string()]).unwrap(),
            vec![SKILL_USE_TOOL_ID.to_string()]
        );
        assert!(resolve_skill_tool_ids(&["skill.nope".to_string()]).is_err());
    }

    #[test]
    fn known_ids_are_unique_and_namespaced() {
        let ids = known_skill_tool_ids();
        for id in ids {
            assert!(id.starts_with("skill."));
        }
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }

    #[test]
    fn skill_tools_are_read_only() {
        use pocopine_agenkit::server::AiTool;
        use pocopine_agenkit_core::ToolSideEffectPolicy::ReadOnly;

        assert_eq!(SkillUseTool::descriptor().side_effect, ReadOnly);
        assert_eq!(SkillReadTool::descriptor().side_effect, ReadOnly);
    }
}
