//! Blog background worker — host-only.

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> pocopine::JobResult<()> {
    use blog as _;
    use pocopine_logging::init_default;

    init_default().map_err(|err| pocopine::JobError::Env(err.to_string()))?;
    tracing::info!(
        target: "pocopine.log",
        "running blog worker (POCOPINE_JOB_BACKEND or POCOPINE_REDIS_URL)"
    );
    pocopine::Worker::from_env()?.run().await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
