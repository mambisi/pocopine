//! The canonical Agenkit example (RFC-093): configure the runtime once, then
//! call a public flow with the same shape as any other Pocopine server work.
//!
//! Run with: `cargo run -p pocopine-agenkit --example summarize`
//!
//! A real app swaps `MockProvider` for a hosted provider (server-only
//! credentials) and exposes `summarize` through a `#[server]` function that
//! calls `agenkit().run_public_flow("summarize", input)`.
//!
//! The runtime is host-only (the browser calls flows through generated
//! `#[server]` helpers, never the runtime directly — §D10), so the example
//! body is gated off `wasm32`; the workspace's wasm build keeps a no-op `main`.

#[cfg(not(target_arch = "wasm32"))]
mod host {
    use pocopine_agenkit::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, schemars::JsonSchema)]
    struct SummarizeInput {
        prompt: String,
    }

    #[derive(Serialize, Deserialize, Debug, schemars::JsonSchema)]
    struct Summary {
        title: String,
        words: u32,
    }

    /// A registered tool the model could call (shown for completeness).
    struct WordCount;

    #[derive(Deserialize, schemars::JsonSchema)]
    struct WordCountInput {
        text: String,
    }

    impl AiTool for WordCount {
        const ID: &'static str = "word_count";
        type Input = WordCountInput;
        type Output = u32;

        fn descriptor() -> ToolDescriptor {
            ToolDescriptor::new("word_count", "Count the words in a string")
        }

        fn call(
            &self,
            input: WordCountInput,
            _ctx: AiToolContext,
        ) -> BoxFuture<'_, AgenkitResult<u32>> {
            Box::pin(async move { Ok(input.text.split_whitespace().count() as u32) })
        }
    }

    /// The app-callable flow: typed input, typed structured output, one trace tree.
    async fn summarize(input: SummarizeInput, ctx: AiFlowContext) -> AgenkitResult<Summary> {
        ctx.ai()
            .system("Summarize the prompt as a title and a word count.")
            .prompt(input.prompt)
            .schema::<Summary>()
            .generate_structured()
            .await
    }

    /// Configure AI once (§D3).
    fn agenkit() -> Agenkit {
        Agenkit::builder()
            .provider(
                MockProvider::new("local")
                    .default_structured(serde_json::json!({"title": "Uploads", "words": 12})),
            )
            .default_model(ModelRef::new("local/default"))
            .tool(WordCount)
            .flow(
                Flow::new("summarize", summarize)
                    .public()
                    .uses_tool("word_count"),
            )
            .build()
            .expect("valid runtime")
    }

    pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
        let summary: Summary = agenkit()
            .run_public_flow(
                "summarize",
                SummarizeInput {
                    prompt: "How do uploads work?".to_string(),
                },
            )
            .await?;
        println!("summary: {summary:?}");
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    host::run().await
}

/// The runtime does not build for `wasm32`; keep the example target compiling.
#[cfg(target_arch = "wasm32")]
fn main() {}
