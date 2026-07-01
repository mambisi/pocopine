use std::path::{Path, PathBuf};

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitResult, ToolDescriptor};

use super::operation::{PatchInput, PatchOutput, apply_prepared, output_for, prepare_patch};
use crate::tools::fs::common::canonical_root;

pub const PATCH_APPLY_TOOL_ID: &str = "patch.apply";

#[derive(Clone, Debug)]
pub struct PatchApplyTool {
    root: PathBuf,
}

impl PatchApplyTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        Ok(Self {
            root: canonical_root(root.as_ref())?,
        })
    }

    pub fn run(&self, input: PatchInput) -> AgenkitResult<PatchOutput> {
        let changes = prepare_patch(&self.root, &input.patch, PATCH_APPLY_TOOL_ID)?;
        apply_prepared(&changes)?;
        Ok(output_for(&changes, true))
    }
}

impl AiTool for PatchApplyTool {
    const ID: &'static str = PATCH_APPLY_TOOL_ID;
    type Input = PatchInput;
    type Output = PatchOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            PATCH_APPLY_TOOL_ID,
            "Apply a validated structured patch inside the project workspace.",
        )
        .side_effecting()
    }

    fn call(
        &self,
        input: Self::Input,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        Box::pin(async move { self.run(input) })
    }
}
