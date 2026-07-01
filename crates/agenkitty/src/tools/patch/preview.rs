use std::path::{Path, PathBuf};

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitResult, ToolDescriptor};

use super::operation::{PatchInput, PatchOutput, output_for, prepare_patch};
use crate::tools::fs::common::canonical_root;

pub const PATCH_PREVIEW_TOOL_ID: &str = "patch.preview";

#[derive(Clone, Debug)]
pub struct PatchPreviewTool {
    root: PathBuf,
}

impl PatchPreviewTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        Ok(Self {
            root: canonical_root(root.as_ref())?,
        })
    }

    pub fn run(&self, input: PatchInput) -> AgenkitResult<PatchOutput> {
        let changes = prepare_patch(&self.root, &input.patch, PATCH_PREVIEW_TOOL_ID)?;
        Ok(output_for(&changes, false))
    }
}

impl AiTool for PatchPreviewTool {
    const ID: &'static str = PATCH_PREVIEW_TOOL_ID;
    type Input = PatchInput;
    type Output = PatchOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            PATCH_PREVIEW_TOOL_ID,
            "Validate a structured patch and summarize affected files without writing.",
        )
    }

    fn call(
        &self,
        input: Self::Input,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        Box::pin(async move { self.run(input) })
    }
}
