#![cfg(not(target_arch = "wasm32"))]
//! Phase 3.2: the in-facade principal adapter threads the caller identity into
//! a flow, its tools/retrieval, and parallel branches.

mod common;

use common::block_on;
use pocopine_agenkit::server::{Agenkit, AiFlowContext, Flow, MockProvider, with_principal};
use pocopine_agenkit_core::{AgenkitResult, ModelRef};
use pocopine_auth::{AuthUser, Principal};

fn whoami_principal(ctx: &AiFlowContext) -> String {
    ctx.principal()
        .user()
        .map(|u| u.id.clone())
        .unwrap_or_else(|| "anon".to_string())
}

async fn whoami(_input: (), ctx: AiFlowContext) -> AgenkitResult<String> {
    Ok(whoami_principal(&ctx))
}

async fn whoami_in_branch(_input: (), ctx: AiFlowContext) -> AgenkitResult<String> {
    let branch_ctx = ctx.clone();
    let results = ctx
        .parallel::<String>("who")
        .branch(async move { Ok(whoami_principal(&branch_ctx)) })
        .run()
        .await?;
    Ok(results[0].clone())
}

fn runtime() -> Agenkit {
    Agenkit::builder()
        .provider(MockProvider::new("local"))
        .default_model(ModelRef::new("local/default"))
        .flow(Flow::new("whoami", whoami).public())
        .flow(Flow::new("whoami_in_branch", whoami_in_branch).public())
        .build()
        .unwrap()
}

fn alice() -> Principal {
    Principal::from_user(AuthUser::new("alice"))
}

#[test]
fn explicit_principal_flows_into_the_flow() {
    let agenkit = runtime();
    let id: String = block_on(agenkit.flow("whoami").principal(alice()).run()).unwrap();
    assert_eq!(id, "alice");
}

#[test]
fn anonymous_when_no_principal() {
    let agenkit = runtime();
    let id: String = block_on(agenkit.flow("whoami").run()).unwrap();
    assert_eq!(id, "anon");
}

#[test]
fn scoped_principal_is_picked_up_by_run_flow() {
    let agenkit = runtime();
    let id: String = block_on(with_principal(alice(), async {
        agenkit.flow("whoami").run::<String>().await
    }))
    .unwrap();
    assert_eq!(id, "alice");
}

#[test]
fn principal_flows_into_a_parallel_branch() {
    let agenkit = runtime();
    let id: String = block_on(agenkit.flow("whoami_in_branch").principal(alice()).run()).unwrap();
    assert_eq!(id, "alice");
}
