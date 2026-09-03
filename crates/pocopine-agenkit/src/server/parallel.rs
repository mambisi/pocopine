//! Bounded parallel fan-out with real in-flight cancellation (RFC-093 Phase
//! 2.6b, §D7, §D8, §D15 DC-7).
//!
//! `ctx.parallel(name)` runs branches concurrently on a `tokio::task::JoinSet`
//! with a [`ParallelJoin`] policy. Unlike `pocopine-jobs` (drop/nack), losing
//! branches are *aborted in flight* via `JoinSet::abort_all` on
//! `FirstSuccess`/`Quorum`. Concurrency, timeout, and min-success are bounded.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tracing::Instrument as _;

use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, FlowStreamEvent, ParallelGroupId, ParallelJoin, StepId, StepKind,
    StepStatus, events,
};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::flow::AiFlowContext;
use super::run::RunState;

type BranchFuture<T> = Pin<Box<dyn Future<Output = AgenkitResult<T>> + Send + 'static>>;

/// Builder for a parallel branch group (`ctx.parallel(...)`).
pub struct ParallelBuilder<'a, T> {
    ctx: &'a AiFlowContext,
    group: String,
    join: ParallelJoin,
    min_success: Option<u32>,
    max_concurrency: Option<usize>,
    timeout: Option<Duration>,
    branches: Vec<(String, BranchFuture<T>)>,
}

impl<'a, T: Send + 'static> ParallelBuilder<'a, T> {
    pub(crate) fn new(ctx: &'a AiFlowContext, group: impl Into<String>) -> Self {
        Self {
            ctx,
            group: group.into(),
            join: ParallelJoin::AllSettled,
            min_success: None,
            max_concurrency: None,
            timeout: None,
            branches: Vec::new(),
        }
    }

    /// Set the join policy (default `AllSettled`).
    pub fn join(mut self, join: ParallelJoin) -> Self {
        self.join = join;
        self
    }

    /// Require at least `n` branches to succeed.
    pub fn min_success(mut self, n: u32) -> Self {
        self.min_success = Some(n);
        self
    }

    /// Cap how many branches run at once.
    pub fn max_concurrency(mut self, n: usize) -> Self {
        self.max_concurrency = Some(n.max(1));
        self
    }

    /// Per-branch timeout (a timed-out branch is a failure).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Add an auto-named branch.
    pub fn branch<F>(mut self, future: F) -> Self
    where
        F: Future<Output = AgenkitResult<T>> + Send + 'static,
    {
        let name = format!("branch-{}", self.branches.len());
        self.branches.push((name, Box::pin(future)));
        self
    }

    /// Add a named branch.
    pub fn named_branch<F>(mut self, name: impl Into<String>, future: F) -> Self
    where
        F: Future<Output = AgenkitResult<T>> + Send + 'static,
    {
        self.branches.push((name.into(), Box::pin(future)));
        self
    }

    /// Run the branches, returning the successful outputs per the join policy.
    pub async fn run(self) -> AgenkitResult<Vec<T>> {
        let run = self.ctx.run_state().clone();
        let group_id = ParallelGroupId::new(self.group.clone());
        let group_step = run.next_step_id();
        let branch_count = self.branches.len() as u32;

        // RFC-123 §4 — the group is one `pocopine.ai.step`; each branch
        // future carries its own, parented here, so overlapping branches
        // show as overlapping spans with no parent bookkeeping.
        let group_span =
            super::spans::step_span_in_group("parallel", &group_step, &self.group, &group_id);
        let result = async move {
            run.emit(
                run.event(
                    events::AI_PARALLEL_STARTED,
                    StepKind::Parallel,
                    StepStatus::Started,
                )
                .with_step(group_step.clone())
                .with_parallel_group(group_id.clone())
                .with_field("group", self.group.clone())
                .with_field("branch_count", branch_count as u64),
            );
            run.stream(FlowStreamEvent::ParallelStarted {
                group_id: group_id.clone(),
                branch_count,
                join: self.join,
            });

            let permits = self
                .max_concurrency
                .unwrap_or_else(|| self.branches.len().max(1));
            let semaphore = Arc::new(Semaphore::new(permits));
            let timeout = self.timeout;

            let mut set: JoinSet<(usize, AgenkitResult<T>)> = JoinSet::new();
            let mut branch_steps: Vec<StepId> = Vec::with_capacity(self.branches.len());
            let mut branch_names: Vec<String> = Vec::with_capacity(self.branches.len());
            // Kept beside the futures: a branch's terminal event and status are
            // recorded here at join time — a panicked or aborted task never
            // reaches its own tail (RFC-123 §4).
            let mut branch_spans: Vec<tracing::Span> = Vec::with_capacity(self.branches.len());
            let mut task_branch: HashMap<tokio::task::Id, usize> =
                HashMap::with_capacity(self.branches.len());

            for (index, (name, future)) in self.branches.into_iter().enumerate() {
                let branch_step = run.next_step_id();
                run.emit(
                    run.event(
                        events::AI_STEP_STARTED,
                        StepKind::Agent,
                        StepStatus::Started,
                    )
                    .with_step(branch_step.clone())
                    .with_parent(group_step.clone())
                    .with_parallel_group(group_id.clone())
                    .with_field("branch", name.clone()),
                );
                run.stream(FlowStreamEvent::BranchStarted {
                    group_id: group_id.clone(),
                    step_id: branch_step.clone(),
                    name: name.clone(),
                });
                let branch_span =
                    super::spans::step_span_in_group("branch", &branch_step, &name, &group_id);
                branch_steps.push(branch_step);
                branch_names.push(name);
                branch_spans.push(branch_span.clone());

                let semaphore = semaphore.clone();
                let handle = set.spawn(
                    async move {
                        let _permit = semaphore
                            .acquire_owned()
                            .await
                            .expect("semaphore is never closed");
                        let result = match timeout {
                            Some(duration) => match tokio::time::timeout(duration, future).await {
                                Ok(result) => result,
                                Err(_) => {
                                    Err(AgenkitError::budget_exhausted("branch exceeded timeout"))
                                }
                            },
                            None => future.await,
                        };
                        (index, result)
                    }
                    .instrument(branch_span),
                );
                task_branch.insert(handle.id(), index);
            }

            let target = match self.join {
                ParallelJoin::FirstSuccess => 1,
                ParallelJoin::Quorum(n) => n.max(1),
                ParallelJoin::All | ParallelJoin::AllSettled => branch_count.max(1),
            };
            // An early-exit join must still satisfy `.min_success(m)`: keep racing
            // until the floor is met instead of aborting the losers and then
            // failing the min-success gate with certainty.
            let target = target.max(self.min_success.unwrap_or(0));

            let mut successes: Vec<T> = Vec::new();
            let mut success_count: u32 = 0;
            let mut first_error: Option<AgenkitError> = None;
            // Track which branches reached a terminal (completed/failed) event so
            // any started-but-undrained branch left behind by `abort_all` can be
            // closed with a cancelled event — otherwise a client reconstructing the
            // tree sees forever-open branches under a completed group (§D7/§D8).
            let mut settled = vec![false; branch_steps.len()];

            while let Some(joined) = set.join_next().await {
                let Some((index, result)) = branch_outcome(joined, &task_branch, &branch_names)
                else {
                    // An aborted loser (cancelled by `abort_all`) carries no branch
                    // outcome — skip it.
                    continue;
                };
                emit_branch_terminal(
                    &run,
                    &group_id,
                    &branch_steps[index],
                    &branch_names[index],
                    &branch_spans[index],
                    &result,
                );
                settled[index] = true;
                match result {
                    Ok(value) => {
                        successes.push(value);
                        success_count += 1;
                        if success_count >= target
                            && matches!(
                                self.join,
                                ParallelJoin::FirstSuccess | ParallelJoin::Quorum(_)
                            )
                        {
                            // Branches that already finished keep their real terminal
                            // (drained here); cancel only the still-running losers in
                            // flight (§D15 DC-7).
                            drain_ready(
                                &mut set,
                                &task_branch,
                                &run,
                                &group_id,
                                BranchRefs {
                                    steps: &branch_steps,
                                    names: &branch_names,
                                    spans: &branch_spans,
                                },
                                &mut settled,
                            );
                            set.abort_all();
                            break;
                        }
                    }
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                        if matches!(self.join, ParallelJoin::All) {
                            drain_ready(
                                &mut set,
                                &task_branch,
                                &run,
                                &group_id,
                                BranchRefs {
                                    steps: &branch_steps,
                                    names: &branch_names,
                                    spans: &branch_spans,
                                },
                                &mut settled,
                            );
                            set.abort_all();
                            break;
                        }
                    }
                }
            }

            // Close any branch that started but was aborted in flight (an
            // early-exit join broke out of the drain loop with losers still
            // running). Each gets a terminal cancelled event so the trace tree has
            // no dangling open branches under the completed group.
            for (index, done) in settled.iter().enumerate() {
                if *done {
                    continue;
                }
                let step_id = branch_steps[index].clone();
                let span = &branch_spans[index];
                span.record("otel.status_code", "ERROR");
                span.record("error.type", "cancelled");
                span.in_scope(|| {
                    run.emit(
                        run.event(
                            events::AI_STEP_CANCELLED,
                            StepKind::Agent,
                            StepStatus::Cancelled,
                        )
                        .with_step(step_id.clone())
                        .with_parallel_group(group_id.clone())
                        .with_field("branch", branch_names[index].clone()),
                    )
                });
                run.stream(FlowStreamEvent::BranchCancelled {
                    group_id: group_id.clone(),
                    step_id,
                });
            }

            run.emit(
                run.event(
                    events::AI_PARALLEL_COMPLETED,
                    StepKind::Parallel,
                    StepStatus::Completed,
                )
                .with_step(group_step)
                .with_parallel_group(group_id.clone())
                .with_field("group", self.group.clone())
                .with_field("success_count", success_count as u64),
            );
            run.stream(FlowStreamEvent::ParallelCompleted {
                group_id,
                success_count,
            });

            // Policy gates.
            if matches!(self.join, ParallelJoin::All)
                && let Some(error) = first_error
            {
                return Err(error);
            }
            // A quorum that can never be reached (e.g. n > branch_count) must fail,
            // not silently return fewer than `n` successes.
            if let ParallelJoin::Quorum(n) = self.join
                && success_count < n
            {
                return Err(AgenkitError::reducer_disagreement(format!(
                    "parallel group `{}` reached {success_count} of the required quorum {n}",
                    self.group
                )));
            }
            if let Some(min) = self.min_success
                && success_count < min
            {
                return Err(AgenkitError::reducer_disagreement(format!(
                    "parallel group `{}` had {success_count} successes, need {min}",
                    self.group
                )));
            }
            if success_count == 0 {
                return Err(first_error.unwrap_or_else(|| {
                    AgenkitError::reducer_disagreement(format!(
                        "parallel group `{}` produced no successful branch",
                        self.group
                    ))
                }));
            }

            Ok(successes)
        }
        .instrument(group_span.clone())
        .await;
        super::spans::close(&group_span, &result);
        result
    }
}

/// Interpret a `JoinSet` result: an aborted (cancelled) loser carries no
/// outcome and is skipped (`None`); a panicked branch is mapped to a branch
/// *failure* so the join policy treats it uniformly.
fn branch_outcome<T>(
    joined: Result<(usize, AgenkitResult<T>), tokio::task::JoinError>,
    task_branch: &HashMap<tokio::task::Id, usize>,
    branch_names: &[String],
) -> Option<(usize, AgenkitResult<T>)> {
    match joined {
        Ok(pair) => Some(pair),
        Err(join_error) if join_error.is_cancelled() => None,
        Err(join_error) => {
            let index = task_branch.get(&join_error.id()).copied()?;
            Some((
                index,
                Err(AgenkitError::internal(format!(
                    "parallel branch `{}` panicked",
                    branch_names[index]
                ))),
            ))
        }
    }
}

/// Emit the terminal trace + stream events for a finished branch
/// (completed/failed).
fn emit_branch_terminal<T>(
    run: &RunState,
    group_id: &ParallelGroupId,
    step_id: &StepId,
    name: &str,
    span: &tracing::Span,
    result: &AgenkitResult<T>,
) {
    super::spans::close(span, result);
    let _inside = span.enter();
    match result {
        Ok(_) => {
            run.emit(
                run.event(
                    events::AI_STEP_COMPLETED,
                    StepKind::Agent,
                    StepStatus::Completed,
                )
                .with_step(step_id.clone())
                .with_parallel_group(group_id.clone())
                .with_field("branch", name.to_string()),
            );
            run.stream(FlowStreamEvent::BranchCompleted {
                group_id: group_id.clone(),
                step_id: step_id.clone(),
            });
        }
        Err(error) => {
            run.emit(
                run.event(events::AI_STEP_FAILED, StepKind::Agent, StepStatus::Failed)
                    .with_step(step_id.clone())
                    .with_parallel_group(group_id.clone())
                    .with_field("branch", name.to_string())
                    .with_error(error.clone()),
            );
            run.stream(FlowStreamEvent::BranchFailed {
                group_id: group_id.clone(),
                step_id: step_id.clone(),
                error_kind: error.kind().to_string(),
            });
        }
    }
}

/// Before an early-exit abort, drain branches whose result is already available
/// (non-blocking) so each keeps its real terminal (completed/failed) instead of
/// being mislabeled `cancelled` by the post-loop closeout. Does not change the
/// success tally — the join target is already met, so drained successes are not
/// added to the result set (preserving `FirstSuccess`/`Quorum` semantics).
/// The per-branch tables `run` keeps in lockstep: step id, name, and the
/// `pocopine.ai.step` span the branch future was instrumented with.
struct BranchRefs<'a> {
    steps: &'a [StepId],
    names: &'a [String],
    spans: &'a [tracing::Span],
}

fn drain_ready<T: 'static>(
    set: &mut JoinSet<(usize, AgenkitResult<T>)>,
    task_branch: &HashMap<tokio::task::Id, usize>,
    run: &RunState,
    group_id: &ParallelGroupId,
    branches: BranchRefs<'_>,
    settled: &mut [bool],
) {
    while let Some(joined) = set.try_join_next() {
        let Some((index, result)) = branch_outcome(joined, task_branch, branches.names) else {
            continue;
        };
        if !settled[index] {
            emit_branch_terminal(
                run,
                group_id,
                &branches.steps[index],
                &branches.names[index],
                &branches.spans[index],
                &result,
            );
            settled[index] = true;
        }
    }
}
