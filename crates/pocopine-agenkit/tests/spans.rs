#![cfg(not(target_arch = "wasm32"))]
//! RFC-123 §4 — the `pocopine.ai.*` span tree opened beside the
//! `TraceEvent` stream: run → step → model, parallel branches parented to
//! their group, and `pocopine.ai.step_id` as the join key to the events.

mod common;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use std::sync::atomic::{AtomicUsize, Ordering};

use common::block_on;
use pocopine_agenkit::server::{
    Agenkit, AgentConfig, AgentEvent, AgentSession, AiFlowContext, AiRetriever, AiTool,
    AiToolContext, BoxFuture, BoxStream, Flow, GenerateRequest, GenerateResponse, MockProvider,
    Provider, ProviderCapabilities, ProviderContext, RetrievalContext, StreamChunk,
};
use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, Content, ModelRef, ParallelJoin, RetrievalHit, RetrievalSet,
    RetrieverDescriptor, SourceRef, ToolCall, ToolDescriptor, Usage,
};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

// ─── capture ────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Span {
    id: u64,
    name: String,
    parent: Option<u64>,
    fields: BTreeMap<String, String>,
}

impl Span {
    fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }
}

#[derive(Clone, Debug)]
struct Ev {
    event_name: Option<String>,
    span: Option<u64>,
    ancestry: Vec<String>,
}

#[derive(Clone, Default)]
struct Capture {
    spans: Arc<Mutex<Vec<Span>>>,
    events: Arc<Mutex<Vec<Ev>>>,
}

impl Capture {
    fn spans(&self) -> Vec<Span> {
        self.spans.lock().unwrap().clone()
    }
    fn named(&self, name: &str) -> Vec<Span> {
        self.spans()
            .into_iter()
            .filter(|s| s.name == name)
            .collect()
    }
    fn one(&self, name: &str) -> Span {
        let mut found = self.named(name);
        assert_eq!(found.len(), 1, "expected one `{name}`: {:?}", self.spans());
        found.remove(0)
    }
    fn by_id(&self, id: u64) -> Span {
        self.spans().into_iter().find(|s| s.id == id).unwrap()
    }
    fn event(&self, name: &str) -> Ev {
        self.events
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.event_name.as_deref() == Some(name))
            .cloned()
            .unwrap_or_else(|| panic!("no `{name}` event"))
    }
}

#[derive(Default)]
struct Visitor(BTreeMap<String, String>);

impl Visit for Visitor {
    fn record_str(&mut self, f: &Field, v: &str) {
        self.0.insert(f.name().into(), v.into());
    }
    fn record_u64(&mut self, f: &Field, v: u64) {
        self.0.insert(f.name().into(), v.to_string());
    }
    fn record_i64(&mut self, f: &Field, v: i64) {
        self.0.insert(f.name().into(), v.to_string());
    }
    fn record_bool(&mut self, f: &Field, v: bool) {
        self.0.insert(f.name().into(), v.to_string());
    }
    fn record_debug(&mut self, f: &Field, v: &dyn fmt::Debug) {
        self.0.insert(f.name().into(), format!("{v:?}"));
    }
}

impl<S> Layer<S> for Capture
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut v = Visitor::default();
        attrs.record(&mut v);
        let parent = ctx
            .span(id)
            .and_then(|s| s.parent().map(|p| p.id().into_u64()));
        self.spans.lock().unwrap().push(Span {
            id: id.into_u64(),
            name: attrs.metadata().name().into(),
            parent,
            fields: v.0,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
        let mut v = Visitor::default();
        values.record(&mut v);
        let mut spans = self.spans.lock().unwrap();
        if let Some(span) = spans.iter_mut().find(|s| s.id == id.into_u64()) {
            span.fields.extend(v.0);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut v = Visitor::default();
        event.record(&mut v);
        let ancestry = ctx
            .event_scope(event)
            .map(|scope| scope.from_root().map(|s| s.name().to_owned()).collect())
            .unwrap_or_default();
        self.events.lock().unwrap().push(Ev {
            event_name: v.0.get("event_name").cloned(),
            span: ctx.event_span(event).map(|s| s.id().into_u64()),
            ancestry,
        });
    }
}

fn capture<F: FnOnce()>(f: F) -> Capture {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    tracing::subscriber::with_default(subscriber, f);
    capture
}

// ─── fixture: retrieve + step + ai, and a parallel fan-out ─────────

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct QuestionInput {
    question: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, schemars::JsonSchema)]
struct Answer {
    text: String,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct DocsQuery {
    question: String,
}

#[derive(Default)]
struct ProjectDocs;

impl AiRetriever for ProjectDocs {
    const ID: &'static str = "project_docs";
    type Query = DocsQuery;

    fn descriptor() -> RetrieverDescriptor {
        RetrieverDescriptor::new("project_docs", "Search project docs").with_source_kinds(["doc"])
    }

    fn retrieve(
        &self,
        query: DocsQuery,
        _ctx: RetrievalContext,
    ) -> BoxFuture<'_, AgenkitResult<RetrievalSet>> {
        Box::pin(async move {
            Ok(RetrievalSet::new(vec![RetrievalHit {
                source: SourceRef::new("doc", "uploads.md"),
                score: Some(0.9),
                content: Content::text(format!("presigned ({})", query.question)),
                citation: None,
            }]))
        })
    }
}

async fn answer_question(input: QuestionInput, ctx: AiFlowContext) -> AgenkitResult<Answer> {
    let docs = ctx
        .retrieve::<ProjectDocs>()
        .query(DocsQuery {
            question: input.question,
        })
        .run()
        .await?;
    let context = ctx
        .step("assemble_context", async move {
            tracing::info!(target: "app::flow", "assembling inside the step");
            Ok::<_, AgenkitError>(docs.hits.len().to_string())
        })
        .await?;
    ctx.ai()
        .system("Answer using the docs.")
        .prompt(context)
        .schema::<Answer>()
        .generate_structured()
        .await
}

async fn fan_out(_input: (), ctx: AiFlowContext) -> AgenkitResult<Vec<i32>> {
    ctx.parallel::<i32>("chunks")
        .join(ParallelJoin::All)
        .named_branch("left", async { Ok(1) })
        .named_branch("right", async { Ok(2) })
        .run()
        .await
}

async fn failing(_input: (), ctx: AiFlowContext) -> AgenkitResult<()> {
    ctx.step("explode", async { Err(AgenkitError::validation("nope")) })
        .await
}

fn runtime() -> Agenkit {
    Agenkit::builder()
        .provider(
            MockProvider::new("local").default_structured(serde_json::json!({"text": "presigned"})),
        )
        .default_model(ModelRef::new("local/default"))
        .retriever(ProjectDocs)
        .flow(
            Flow::new("answer_question", answer_question)
                .public()
                .uses_retriever("project_docs"),
        )
        .flow(Flow::new("fan_out", fan_out).public())
        .flow(Flow::new("failing", failing).public())
        .build()
        .unwrap()
}

// ─── tests ──────────────────────────────────────────────────────────

#[test]
fn flow_run_step_and_model_form_one_span_tree() {
    let cap = capture(|| {
        let answer: Answer = block_on(
            runtime()
                .flow("answer_question")
                .input(QuestionInput {
                    question: "uploads?".into(),
                })
                .run(),
        )
        .unwrap();
        assert_eq!(answer.text, "presigned");
    });

    let run = cap.one("pocopine.ai.run");
    assert_eq!(run.parent, None);
    assert_eq!(run.field("otel.kind"), Some("internal"));
    assert_eq!(run.field("pocopine.ai.flow"), Some("answer_question"));
    assert!(
        run.field("pocopine.ai.run_id")
            .is_some_and(|v| v.starts_with("run-"))
    );
    assert!(
        run.field("pocopine.ai.trace_id")
            .is_some_and(|v| v.starts_with("trace-"))
    );
    assert_eq!(run.field("otel.status_code"), Some("OK"));

    let steps = cap.named("pocopine.ai.step");
    assert_eq!(steps.len(), 2, "{steps:?}");
    let retrieval = steps
        .iter()
        .find(|s| s.field("pocopine.ai.step_kind") == Some("retrieval"))
        .expect("retrieval step");
    assert_eq!(retrieval.parent, Some(run.id));
    assert_eq!(
        retrieval.field("pocopine.ai.step_name"),
        Some("project_docs")
    );
    assert_eq!(retrieval.field("otel.status_code"), Some("OK"));
    let custom = steps
        .iter()
        .find(|s| s.field("pocopine.ai.step_kind") == Some("custom"))
        .expect("custom step");
    assert_eq!(custom.parent, Some(run.id));
    assert_eq!(
        custom.field("pocopine.ai.step_name"),
        Some("assemble_context")
    );
    assert!(
        custom
            .field("pocopine.ai.step_id")
            .is_some_and(|v| v.starts_with('s'))
    );

    let model = cap.one("pocopine.ai.model");
    assert_eq!(
        model.parent,
        Some(run.id),
        "ctx.ai() at flow level parents to the run"
    );
    assert_eq!(model.field("otel.kind"), Some("client"));
    assert_eq!(model.field("gen_ai.operation.name"), Some("chat"));
    assert_eq!(model.field("gen_ai.provider.name"), Some("local"));
    assert_eq!(model.field("gen_ai.request.model"), Some("local/default"));
    assert_eq!(model.field("otel.status_code"), Some("OK"));
    assert!(
        model.field("pocopine.ai.step_id").is_some(),
        "join key recorded"
    );

    // Every step id on a span is unique — the join key is unambiguous.
    let ids: Vec<&str> = cap
        .spans()
        .iter()
        .filter_map(|s| s.fields.get("pocopine.ai.step_id"))
        .map(String::as_str)
        .map(|s| Box::leak(s.to_owned().into_boxed_str()) as &str)
        .collect();
    let unique: std::collections::BTreeSet<&str> = ids.iter().copied().collect();
    assert_eq!(ids.len(), unique.len());

    // Events land inside the matching spans — the stream did not move.
    assert_eq!(cap.event("ai_flow_completed").ancestry, ["pocopine.ai.run"]);
    assert_eq!(
        cap.event("ai_model_response").ancestry,
        ["pocopine.ai.run", "pocopine.ai.model"]
    );
    assert_eq!(
        cap.event("ai_step_completed").ancestry,
        ["pocopine.ai.run", "pocopine.ai.step"]
    );
    assert_eq!(
        cap.event("ai_retrieval_started").ancestry,
        ["pocopine.ai.run", "pocopine.ai.step"]
    );
    // App events inside a step nest for free.
    let inside = cap
        .events
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.ancestry.len() == 2 && e.ancestry[1] == "pocopine.ai.step")
        .cloned()
        .expect("app event inside the custom step");
    assert_eq!(cap.by_id(inside.span.unwrap()).name, "pocopine.ai.step");

    // Nothing sensitive: no prompt, doc text, or answer on any span.
    for span in cap.spans() {
        for value in span.fields.values() {
            assert!(!value.contains("presigned"), "{span:?}");
            assert!(!value.contains("Answer using"), "{span:?}");
        }
    }
}

#[test]
fn parallel_branches_are_children_of_their_group() {
    let cap = capture(|| {
        let out: Vec<i32> = block_on(runtime().flow("fan_out").run()).unwrap();
        assert_eq!(out.len(), 2);
    });

    let run = cap.one("pocopine.ai.run");
    let steps = cap.named("pocopine.ai.step");
    let group = steps
        .iter()
        .find(|s| s.field("pocopine.ai.step_kind") == Some("parallel"))
        .expect("group step");
    assert_eq!(group.parent, Some(run.id));
    assert_eq!(group.field("pocopine.ai.step_name"), Some("chunks"));
    assert_eq!(group.field("pocopine.ai.parallel_group_id"), Some("chunks"));
    assert_eq!(group.field("otel.status_code"), Some("OK"));

    let branches: Vec<&Span> = steps
        .iter()
        .filter(|s| s.field("pocopine.ai.step_kind") == Some("branch"))
        .collect();
    assert_eq!(branches.len(), 2, "{steps:?}");
    for branch in branches {
        assert_eq!(branch.parent, Some(group.id));
        assert_eq!(
            branch.field("pocopine.ai.parallel_group_id"),
            Some("chunks")
        );
        assert_eq!(branch.field("otel.status_code"), Some("OK"));
        assert!(matches!(
            branch.field("pocopine.ai.step_name"),
            Some("left" | "right")
        ));
    }

    // Branch terminal events are recorded at join time, inside the branch.
    let branch_completed: Vec<Ev> = cap
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.event_name.as_deref() == Some("ai_step_completed"))
        .cloned()
        .collect();
    assert_eq!(branch_completed.len(), 2, "{branch_completed:?}");
    for ev in branch_completed {
        assert_eq!(
            ev.ancestry,
            ["pocopine.ai.run", "pocopine.ai.step", "pocopine.ai.step"],
            "{ev:?}"
        );
    }
}

#[test]
fn streamed_flow_keeps_the_callers_span_as_parent() {
    use tracing::Instrument as _;

    let cap = capture(|| {
        block_on(
            async {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let out = runtime()
                    .flow("answer_question")
                    .input(QuestionInput {
                        question: "uploads?".into(),
                    })
                    .stream_into(tx)
                    .await
                    .unwrap();
                assert_eq!(out["text"], "presigned");
                while rx.recv().await.is_some() {}
            }
            .instrument(tracing::info_span!("caller")),
        );
    });
    let caller = cap.one("caller");
    let run = cap.one("pocopine.ai.run");
    assert_eq!(run.parent, Some(caller.id), "ai.run hangs from the caller");
}

#[test]
fn failed_step_closes_step_and_run_as_error() {
    let cap = capture(|| {
        let err = block_on(runtime().flow("failing").run::<()>()).unwrap_err();
        assert_eq!(err.kind(), "validation");
    });
    let step = cap.one("pocopine.ai.step");
    assert_eq!(step.field("otel.status_code"), Some("ERROR"));
    assert_eq!(step.field("error.type"), Some("validation"));
    let run = cap.one("pocopine.ai.run");
    assert_eq!(run.field("otel.status_code"), Some("ERROR"));
    assert_eq!(run.field("error.type"), Some("validation"));
}

// ─── conversational runtime: turn → model + tool ───────────────────

#[derive(Deserialize, schemars::JsonSchema)]
struct NoInput {}

#[derive(Serialize, schemars::JsonSchema)]
struct Done {
    ok: bool,
}

struct Lookup;

impl AiTool for Lookup {
    const ID: &'static str = "lookup";
    type Input = NoInput;
    type Output = Done;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new("lookup", "Looks something up")
    }

    fn call(&self, _input: NoInput, _ctx: AiToolContext) -> BoxFuture<'_, AgenkitResult<Done>> {
        Box::pin(async move {
            tracing::info!(target: "app::tool", "inside the tool");
            Ok(Done { ok: true })
        })
    }
}

/// First call asks for the `lookup` tool, second call answers.
struct CallsLookupThenAnswers {
    calls: AtomicUsize,
}

impl Provider for CallsLookupThenAnswers {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: false,
            json_schema: true,
            tools: true,
        }
    }

    fn generate<'a>(
        &'a self,
        _request: GenerateRequest,
        _cx: &'a ProviderContext,
    ) -> BoxFuture<'a, AgenkitResult<GenerateResponse>> {
        let first = self.calls.fetch_add(1, Ordering::Relaxed) == 0;
        Box::pin(async move {
            let mut response = if first {
                GenerateResponse {
                    tool_calls: vec![ToolCall::new("call-1", "lookup", serde_json::json!({}))],
                    ..GenerateResponse::text("")
                }
            } else {
                GenerateResponse::text("found it")
            };
            response.usage = Some(Usage::new(100, 20));
            Ok(response)
        })
    }

    fn generate_stream<'a>(
        &'a self,
        _request: GenerateRequest,
        _cx: &'a ProviderContext,
    ) -> BoxStream<'a, AgenkitResult<StreamChunk>> {
        Box::pin(futures::stream::empty())
    }
}

#[test]
fn conversational_turn_parents_model_and_tool_spans_and_sums_usage() {
    let agenkit = Agenkit::builder()
        .provider(CallsLookupThenAnswers {
            calls: AtomicUsize::new(0),
        })
        .default_model(ModelRef::new("anthropic/claude-sonnet-4-6"))
        .tool(Lookup)
        .build()
        .unwrap();

    let cap = capture(|| {
        block_on(async {
            let session = AgentSession::builder(&agenkit)
                .config(AgentConfig::new().tools(["lookup"]))
                .open(None)
                .await
                .unwrap();
            let mut stream = session.prompt("look it up");
            let mut last = None;
            while let Some(ev) = stream.next().await {
                last = Some(ev);
            }
            assert!(matches!(last, Some(AgentEvent::Stopped { .. })), "{last:?}");
        });
    });

    let turn = cap.one("pocopine.ai.turn");
    assert_eq!(turn.parent, None);
    assert_eq!(
        turn.field("gen_ai.request.model"),
        Some("anthropic/claude-sonnet-4-6")
    );
    assert_eq!(turn.field("otel.status_code"), Some("OK"));
    assert_eq!(
        turn.field("gen_ai.usage.input_tokens"),
        Some("200"),
        "two calls summed"
    );
    assert_eq!(turn.field("gen_ai.usage.output_tokens"), Some("40"));

    let models = cap.named("pocopine.ai.model");
    assert_eq!(models.len(), 2, "{models:?}");
    for model in &models {
        assert_eq!(model.parent, Some(turn.id));
        assert_eq!(model.field("gen_ai.provider.name"), Some("anthropic"));
        assert_eq!(model.field("gen_ai.usage.input_tokens"), Some("100"));
        assert_eq!(model.field("gen_ai.usage.output_tokens"), Some("20"));
        assert_eq!(model.field("otel.status_code"), Some("OK"));
        assert!(model.field("pocopine.ai.step_id").is_some());
    }

    let tool = cap.one("pocopine.ai.tool");
    assert_eq!(tool.parent, Some(turn.id));
    assert_eq!(tool.field("gen_ai.tool.name"), Some("lookup"));
    assert_eq!(tool.field("otel.status_code"), Some("OK"));
    assert!(tool.field("pocopine.ai.step_id").is_some());
    assert!(
        !tool.fields.values().any(|v| v.contains("call-1")),
        "no call payload on the span"
    );

    assert_eq!(
        cap.event("ai_tool_completed").ancestry,
        ["pocopine.ai.turn", "pocopine.ai.tool"]
    );
    assert_eq!(
        cap.event("ai_model_request").ancestry,
        ["pocopine.ai.turn", "pocopine.ai.model"]
    );
    assert_eq!(cap.event("ai_step_started").ancestry, ["pocopine.ai.turn"]);
    let inside_tool = cap
        .events
        .lock()
        .unwrap()
        .iter()
        .find(|e| {
            e.ancestry.last().map(String::as_str) == Some("pocopine.ai.tool")
                && e.event_name.is_none()
        })
        .cloned()
        .expect("the app event inside the tool");
    assert_eq!(
        inside_tool.ancestry,
        ["pocopine.ai.turn", "pocopine.ai.tool"]
    );
}
