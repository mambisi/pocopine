#![cfg(not(target_arch = "wasm32"))]
//! Phase 2.6b: parallel fan-out + reducer (`answer_with_review`), real
//! in-flight cancellation on `FirstSuccess`, and public stream emission.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::block_on;
use pocopine_agenkit::server::{
    Agenkit, AiAgent, AiAgentBuilder, AiFlowContext, Flow, MockProvider,
};
use pocopine_agenkit_core::{AgenkitResult, FlowStreamEvent, ModelRef, ParallelJoin};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct Question {
    q: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, schemars::JsonSchema)]
struct CandidateAnswer {
    claim: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, schemars::JsonSchema)]
struct FinalAnswer {
    answer: String,
}

struct Researcher;

impl AiAgent for Researcher {
    const ID: &'static str = "researcher";
    type Input = Question;
    type Output = CandidateAnswer;

    fn configure(builder: AiAgentBuilder<Self>) -> AiAgentBuilder<Self> {
        builder.system("Answer concisely.")
    }
}

async fn answer_with_review(input: Question, ctx: AiFlowContext) -> AgenkitResult<FinalAnswer> {
    let candidates = ctx
        .parallel::<CandidateAnswer>("candidate_answers")
        .join(ParallelJoin::AllSettled)
        .min_success(2)
        .max_concurrency(3)
        .branch(
            ctx.agent::<Researcher>()
                .input(Question {
                    q: format!("{}#1", input.q),
                })
                .run(),
        )
        .branch(
            ctx.agent::<Researcher>()
                .input(Question {
                    q: format!("{}#2", input.q),
                })
                .run(),
        )
        .branch(
            ctx.agent::<Researcher>()
                .input(Question {
                    q: format!("{}#3", input.q),
                })
                .run(),
        )
        .run()
        .await?;

    ctx.reduce("judge_and_merge", candidates)
        .system("Keep only claims supported by evidence.")
        .schema::<FinalAnswer>()
        .await
}

async fn race(_input: (), ctx: AiFlowContext) -> AgenkitResult<i32> {
    let loser_done = ctx.state::<AtomicBool>("loser_done")?;
    let winners = ctx
        .parallel::<i32>("race")
        .join(ParallelJoin::FirstSuccess)
        // Fast branch wins immediately.
        .branch(async { Ok(1) })
        // Slow branch must be aborted before it can set the flag.
        .branch({
            let loser_done = loser_done.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(400)).await;
                loser_done.store(true, Ordering::SeqCst);
                Ok(2)
            }
        })
        .run()
        .await?;
    Ok(winners[0])
}

async fn strict_with_panic(_input: (), ctx: AiFlowContext) -> AgenkitResult<Vec<i32>> {
    ctx.parallel::<i32>("strict")
        .join(ParallelJoin::All)
        .branch(async { Ok(1) })
        .branch(async { panic!("branch blew up") })
        .branch(async { Ok(3) })
        .run()
        .await
}

async fn settled_with_panic(_input: (), ctx: AiFlowContext) -> AgenkitResult<Vec<i32>> {
    ctx.parallel::<i32>("settled")
        .join(ParallelJoin::AllSettled)
        .branch(async { Ok(1) })
        .branch(async { panic!("branch blew up") })
        .branch(async { Ok(3) })
        .run()
        .await
}

fn runtime() -> (Agenkit, Arc<AtomicBool>) {
    let loser_done = Arc::new(AtomicBool::new(false));
    let agenkit = Agenkit::builder()
        .provider(
            MockProvider::new("local")
                .on_prompt_structured("Candidate answers", serde_json::json!({"answer": "merged"}))
                .default_structured(serde_json::json!({"claim": "c"})),
        )
        .default_model(ModelRef::new("local/default"))
        .state_arc("loser_done", loser_done.clone())
        .flow(Flow::new("answer_with_review", answer_with_review).uses_agent("researcher"))
        .flow(Flow::new("race", race).uses_state("loser_done"))
        .flow(Flow::new("strict_with_panic", strict_with_panic))
        .flow(Flow::new("settled_with_panic", settled_with_panic))
        .build()
        .unwrap();
    (agenkit, loser_done)
}

#[test]
fn answer_with_review_fans_out_then_reduces() {
    let (agenkit, _) = runtime();
    let answer: FinalAnswer = block_on(
        agenkit
            .flow("answer_with_review")
            .input(Question {
                q: "how do uploads work?".to_string(),
            })
            .run(),
    )
    .unwrap();
    // Three agent branches (AllSettled, min_success 2) reduced by a model judge.
    assert_eq!(
        answer,
        FinalAnswer {
            answer: "merged".to_string()
        }
    );
}

#[test]
fn first_success_aborts_losing_branch_in_flight() {
    let (agenkit, loser_done) = runtime();
    let winner: i32 = block_on(agenkit.flow("race").run()).unwrap();
    assert_eq!(winner, 1);
    // The slow branch was aborted mid-sleep, so it never reached the store.
    assert!(
        !loser_done.load(Ordering::SeqCst),
        "losing branch was not cancelled in flight"
    );
}

#[test]
fn all_join_fails_when_a_branch_panics() {
    // A panicked branch is a branch failure, not a silent skip: `All` must
    // refuse to return a partial result set.
    let (agenkit, _) = runtime();
    let result: Result<Vec<i32>, _> = block_on(agenkit.flow("strict_with_panic").run());
    let error = result.expect_err("All join must fail on a panicked branch");
    assert_eq!(error.kind(), "internal", "error: {error}");
}

#[test]
fn all_settled_join_records_a_panicked_branch_but_keeps_survivors() {
    let (agenkit, _) = runtime();
    let mut survivors: Vec<i32> = block_on(agenkit.flow("settled_with_panic").run()).unwrap();
    // Settled results arrive in completion order; only membership matters.
    survivors.sort_unstable();
    assert_eq!(survivors, vec![1, 3]);
}

#[test]
fn first_success_emits_cancelled_terminal_for_aborted_loser() {
    // The losing branch is aborted in flight by `FirstSuccess`. It emitted a
    // `BranchStarted`, so the stream must close it with a terminal
    // `BranchCancelled` — never leave it forever-open under the completed group.
    let (agenkit, _) = runtime();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let winner: i32 = block_on(async {
        let value = agenkit
            .flow("race")
            .input(serde_json::json!(null))
            .stream(tx)
            .await
            .unwrap();
        serde_json::from_value(value).unwrap()
    });
    assert_eq!(winner, 1);

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    let count = |pred: fn(&FlowStreamEvent) -> bool| events.iter().filter(|e| pred(e)).count();

    assert_eq!(
        count(|e| matches!(e, FlowStreamEvent::BranchStarted { .. })),
        2,
        "both branches started"
    );
    assert_eq!(
        count(|e| matches!(e, FlowStreamEvent::BranchCompleted { .. })),
        1,
        "the winner completed"
    );
    assert_eq!(
        count(|e| matches!(e, FlowStreamEvent::BranchCancelled { .. })),
        1,
        "the aborted loser must be closed with a cancelled terminal"
    );
    // Every started branch reached exactly one terminal: no dangling open branch.
    let started = count(|e| matches!(e, FlowStreamEvent::BranchStarted { .. }));
    let terminal = count(|e| {
        matches!(
            e,
            FlowStreamEvent::BranchCompleted { .. }
                | FlowStreamEvent::BranchFailed { .. }
                | FlowStreamEvent::BranchCancelled { .. }
        )
    });
    assert_eq!(
        started, terminal,
        "every branch must reach a terminal event"
    );
}

#[test]
fn streaming_emits_public_flow_events() {
    let (agenkit, _) = runtime();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let answer: FinalAnswer = block_on(async {
        let value = agenkit
            .flow("answer_with_review")
            .input(serde_json::json!({"q": "how do uploads work?"}))
            .stream(tx)
            .await
            .unwrap();
        serde_json::from_value(value).unwrap()
    });
    assert_eq!(
        answer,
        FinalAnswer {
            answer: "merged".to_string()
        }
    );

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    // The public stream reconstructs the trace tree for clients: flow ->
    // parallel(group) -> 3 branches -> reducer -> flow complete.
    let has = |pred: fn(&FlowStreamEvent) -> bool| events.iter().any(pred);
    assert!(has(|e| matches!(e, FlowStreamEvent::FlowStarted { .. })));
    assert!(has(|e| matches!(
        e,
        FlowStreamEvent::ParallelStarted { .. }
    )));
    assert!(has(|e| matches!(
        e,
        FlowStreamEvent::ParallelCompleted { .. }
    )));
    assert!(has(|e| matches!(e, FlowStreamEvent::ReducerStarted { .. })));
    assert!(has(|e| matches!(
        e,
        FlowStreamEvent::ReducerCompleted { .. }
    )));
    assert!(has(|e| matches!(e, FlowStreamEvent::FlowCompleted { .. })));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, FlowStreamEvent::BranchStarted { .. }))
            .count(),
        3,
        "expected three parallel branches"
    );

    // No public stream event may carry a raw prompt, tool args, or provider
    // payload (§D8/§D10). Serialize every event and scan for the seeded marker.
    for event in &events {
        let json = serde_json::to_string(event).unwrap().to_lowercase();
        for needle in [
            "how do uploads work",
            "system",
            "prompt",
            "api_key",
            "candidate answers",
        ] {
            assert!(
                !json.contains(needle),
                "stream event leaked `{needle}`: {json}"
            );
        }
    }
}
