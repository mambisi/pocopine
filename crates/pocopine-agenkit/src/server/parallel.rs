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

use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, FlowStreamEvent, ParallelGroupId, ParallelJoin, StepId, StepKind,
    StepStatus, events,
};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::flow::AiFlowContext;

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
            branch_steps.push(branch_step);
            branch_names.push(name);

            let semaphore = semaphore.clone();
            let handle = set.spawn(async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .expect("semaphore is never closed");
                let result = match timeout {
                    Some(duration) => match tokio::time::timeout(duration, future).await {
                        Ok(result) => result,
                        Err(_) => Err(AgenkitError::budget_exhausted("branch exceeded timeout")),
                    },
                    None => future.await,
                };
                (index, result)
            });
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
            let (index, result) = match joined {
                Ok(pair) => pair,
                // An aborted loser (cancelled by `abort_all`) is expected and
                // carries no branch outcome — skip it.
                Err(join_error) if join_error.is_cancelled() => continue,
                // A panicked branch is a branch FAILURE, not a no-op: surface
                // it through the normal failure path so `All` aborts the group
                // and `AllSettled` records it instead of returning partial Ok.
                Err(join_error) => {
                    let Some(index) = task_branch.get(&join_error.id()).copied() else {
                        continue;
                    };
                    (
                        index,
                        Err(AgenkitError::internal(format!(
                            "parallel branch `{}` panicked",
                            branch_names[index]
                        ))),
                    )
                }
            };
            let step_id = branch_steps[index].clone();
            let name = branch_names[index].clone();
            match result {
                Ok(value) => {
                    run.emit(
                        run.event(
                            events::AI_STEP_COMPLETED,
                            StepKind::Agent,
                            StepStatus::Completed,
                        )
                        .with_step(step_id.clone())
                        .with_parallel_group(group_id.clone())
                        .with_field("branch", name),
                    );
                    run.stream(FlowStreamEvent::BranchCompleted {
                        group_id: group_id.clone(),
                        step_id,
                    });
                    settled[index] = true;
                    successes.push(value);
                    success_count += 1;
                    if success_count >= target
                        && matches!(
                            self.join,
                            ParallelJoin::FirstSuccess | ParallelJoin::Quorum(_)
                        )
                    {
                        // Cancel the losing branches in flight (§D15 DC-7).
                        set.abort_all();
                        break;
                    }
                }
                Err(error) => {
                    run.emit(
                        run.event(events::AI_STEP_FAILED, StepKind::Agent, StepStatus::Failed)
                            .with_step(step_id.clone())
                            .with_parallel_group(group_id.clone())
                            .with_field("branch", name)
                            .with_error(error.clone()),
                    );
                    run.stream(FlowStreamEvent::BranchFailed {
                        group_id: group_id.clone(),
                        step_id,
                        error_kind: error.kind().to_string(),
                    });
                    settled[index] = true;
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    if matches!(self.join, ParallelJoin::All) {
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
            run.emit(
                run.event(
                    events::AI_STEP_CANCELLED,
                    StepKind::Agent,
                    StepStatus::Cancelled,
                )
                .with_step(step_id.clone())
                .with_parallel_group(group_id.clone())
                .with_field("branch", branch_names[index].clone()),
            );
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
}
