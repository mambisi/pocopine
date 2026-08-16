//! `skill.read`: a confined, bounded window of an activated skill's bundled
//! resource file (level 3).

use std::sync::Arc;

use agenkitty_skills::ReadOpts;
use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{SkillRuntime, map_skill_error};

pub const SKILL_READ_TOOL_ID: &str = "skill.read";

#[derive(Clone)]
pub struct SkillReadTool {
    runtime: Arc<SkillRuntime>,
}

impl SkillReadTool {
    pub fn new(runtime: Arc<SkillRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: SkillReadInput) -> AgenkitResult<SkillReadOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let catalog = self.runtime.catalog();
        let Some(skill) = self.runtime.visible_skill(&catalog, &context, &input.name) else {
            return Err(AgenkitError::not_found(format!(
                "skill `{}` not found",
                input.name
            )));
        };
        if !self
            .runtime
            .is_activated(&context.activation_key(), &input.name)
        {
            return Err(AgenkitError::tool_policy(format!(
                "skill `{}` is not active in this session — call skill.use first",
                input.name
            )));
        }
        let digest = skill.digest_short();
        let chunk = catalog
            .read_resource(
                &input.name,
                &input.path,
                ReadOpts {
                    offset: input.offset,
                    limit: input.limit,
                },
            )
            .map_err(map_skill_error)?;
        tracing::info!(
            target: "pocopine.log",
            skill = %input.name,
            digest = %digest,
            bytes_read = chunk.bytes_read,
            eof = chunk.eof,
            "skill resource read"
        );
        Ok(SkillReadOutput {
            content: chunk.content,
            bytes_read: chunk.bytes_read,
            file_size: chunk.file_size,
            eof: chunk.eof,
        })
    }
}

impl AiTool for SkillReadTool {
    const ID: &'static str = SKILL_READ_TOOL_ID;
    type Input = SkillReadInput;
    type Output = SkillReadOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            SKILL_READ_TOOL_ID,
            "Read a bundled resource file of a skill previously loaded with skill.use. Paths are \
             relative to the skill directory and confined to it. Supports byte-window paging via \
             `offset` and `limit`.",
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
pub struct SkillReadInput {
    /// The activated skill's name.
    pub name: String,
    /// Resource path relative to the skill directory (as returned by
    /// skill.use).
    pub path: String,
    /// Byte offset to start reading from. Default 0.
    #[serde(default)]
    pub offset: Option<u64>,
    /// Window size in bytes; clamped to the configured maximum.
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct SkillReadOutput {
    /// Sanitized text of the window. Non-UTF-8 bytes are replaced.
    pub content: String,
    pub bytes_read: usize,
    pub file_size: u64,
    /// Whether this window reached the end of the file.
    pub eof: bool,
}
