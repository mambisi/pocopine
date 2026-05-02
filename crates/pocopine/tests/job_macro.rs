use pocopine::JobResult;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JobPayload {
    value: String,
}

#[pocopine::job(queue = "tests", retries = 2)]
async fn macro_registered_job(input: JobPayload) -> JobResult<()> {
    assert_eq!(input.value, "ok");
    Ok(())
}

#[pocopine::job(queue = "periodic", every = "15m", retries = 1)]
async fn refresh_periodic_cache() -> JobResult<()> {
    Ok(())
}

#[pocopine::job(queue = "periodic", cron = "0 0 2 * * * *")]
async fn nightly_periodic_cleanup() -> JobResult<()> {
    Ok(())
}

#[test]
fn job_descriptor_is_registered_by_inventory() {
    let descriptor = pocopine::jobs::registered_jobs()
        .find(|descriptor| descriptor.name.ends_with("::macro_registered_job"))
        .expect("job descriptor should be registered");

    assert_eq!(descriptor.queue, "tests");
    assert_eq!(descriptor.retry_policy.max_attempts, 3);
}

#[test]
fn periodic_job_descriptor_is_registered_by_inventory() {
    let descriptor = pocopine::jobs::registered_jobs()
        .find(|descriptor| descriptor.name.ends_with("::refresh_periodic_cache"))
        .expect("periodic job descriptor should be registered");

    assert_eq!(descriptor.queue, "periodic");
    assert_eq!(descriptor.retry_policy.max_attempts, 2);
    assert_eq!(
        descriptor.periodic,
        Some(pocopine::PeriodicSchedule::Every {
            interval_ms: 15 * 60 * 1_000
        })
    );

    let descriptor = pocopine::jobs::registered_jobs()
        .find(|descriptor| descriptor.name.ends_with("::nightly_periodic_cleanup"))
        .expect("cron job descriptor should be registered");

    assert_eq!(
        descriptor.periodic,
        Some(pocopine::PeriodicSchedule::Cron {
            expr: "0 0 2 * * * *"
        })
    );
}

#[test]
fn generated_dispatch_decodes_payload() {
    let payload = serde_json::to_vec(&JobPayload { value: "ok".into() }).unwrap();
    let future = macro_registered_job_job::__pocopine_dispatch_macro_registered_job(payload);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(future).unwrap();
}

#[test]
fn generated_periodic_dispatch_decodes_unit_payload() {
    let payload = serde_json::to_vec(&()).unwrap();
    let future = refresh_periodic_cache_job::__pocopine_dispatch_refresh_periodic_cache(payload);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(future).unwrap();
}
