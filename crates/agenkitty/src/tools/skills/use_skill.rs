//! `skill.use`: load a skill's full instructions (level 2) and unlock
//! `skill.read` for its bundled files.

use std::sync::Arc;

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{SkillRuntime, map_skill_error};

pub const SKILL_USE_TOOL_ID: &str = "skill.use";

#[derive(Clone)]
pub struct SkillUseTool {
    runtime: Arc<SkillRuntime>,
}

impl SkillUseTool {
    pub fn new(runtime: Arc<SkillRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: SkillUseInput) -> AgenkitResult<SkillUseOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let catalog = self.runtime.catalog();
        // Out-of-view and nonexistent names return the same shape (S7).
        let Some(skill) = self.runtime.visible_skill(&catalog, &context, &input.name) else {
            return Err(AgenkitError::not_found(format!(
                "skill `{}` not found",
                input.name
            )));
        };
        // This tool IS the model-invocation path; hosts invoke through the
        // library directly, so the frontmatter rule binds here.
        if skill.ext.disable_model_invocation {
            return Err(AgenkitError::tool_policy(format!(
                "skill `{}` is marked disable-model-invocation and can only be invoked by the host",
                input.name
            )));
        }
        let digest = skill.digest_short();
        let body = catalog
            .body(&input.name, input.arguments.as_deref())
            .map_err(map_skill_error)?;
        self.runtime
            .activate(&context.activation_key(), &input.name);
        // Observability per RFC-069: name/digest/outcome/sizes only — never
        // the body or the arguments.
        tracing::info!(
            target: "pocopine.log",
            skill = %body.name,
            digest = %digest,
            body_bytes = body.body.len(),
            truncated = body.truncated,
            resources = body.resources.len(),
            "skill activated"
        );
        Ok(SkillUseOutput {
            name: body.name,
            body: body.body,
            truncated: body.truncated,
            resources: body.resources,
        })
    }
}

impl AiTool for SkillUseTool {
    const ID: &'static str = SKILL_USE_TOOL_ID;
    type Input = SkillUseInput;
    type Output = SkillUseOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            SKILL_USE_TOOL_ID,
            "Load a skill's full instructions by name (the available skills are listed in the \
             system prompt). Returns the instruction body plus the relative paths of bundled \
             resource files, and unlocks skill.read for them. Pass optional `arguments` for \
             skills that take them.",
        )
    }

    fn call(
        &self,
        input: Self::Input,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        Box::pin(async move { self.run(input).await })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct SkillUseInput {
    /// The skill name as listed in the system-prompt index.
    pub name: String,
    /// Optional argument string, substituted into `$ARGUMENTS`-style
    /// placeholders in the skill body.
    #[serde(default)]
    pub arguments: Option<String>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct SkillUseOutput {
    pub name: String,
    /// The sanitized, bounded instruction body.
    pub body: String,
    /// Whether the body was cut at the configured byte limit.
    pub truncated: bool,
    /// Relative paths readable via skill.read.
    pub resources: Vec<String>,
}
