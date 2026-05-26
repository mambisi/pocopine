#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pocopine_storage::backend_common::{UploadSessionLockCleanup, UploadSessionLockRegistry};
use pocopine_storage::{StorageResult, UploadSessionId};

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "run by storage-stress CI"]
async fn session_lock_registry_serializes_under_stress_and_panic() -> StorageResult<()> {
    const SESSIONS: usize = 32;
    const WORKERS: usize = 96;
    const ROUNDS: usize = 96;

    let registry = Arc::new(UploadSessionLockRegistry::new());
    let active = Arc::new(
        (0..SESSIONS)
            .map(|_| AtomicUsize::new(0))
            .collect::<Vec<_>>(),
    );
    let violations = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();

    for worker in 0..WORKERS {
        let registry = Arc::clone(&registry);
        let active = Arc::clone(&active);
        let violations = Arc::clone(&violations);
        handles.push(tokio::spawn(async move {
            for round in 0..ROUNDS {
                let session_index = ((worker * 31) + (round * 17)) % SESSIONS;
                let session = UploadSessionId::new(format!("stress-{session_index}"))?;
                let lock = registry.lock(&session);
                let _cleanup = UploadSessionLockCleanup::new(&registry, &session);
                let _guard = lock.lock().await;

                if active[session_index].fetch_add(1, Ordering::SeqCst) != 0 {
                    violations.fetch_add(1, Ordering::SeqCst);
                }
                tokio::task::yield_now().await;
                active[session_index].fetch_sub(1, Ordering::SeqCst);
            }
            Ok::<_, pocopine_storage::StorageError>(())
        }));
    }

    for handle in handles {
        handle.await.expect("stress worker panicked")?;
    }
    assert_eq!(
        violations.load(Ordering::SeqCst),
        0,
        "session locks allowed concurrent holders for the same session"
    );

    let panic_session = UploadSessionId::new("panic-holder")?;
    let panic_registry = Arc::clone(&registry);
    let panic_session_for_task = panic_session.clone();
    let panic_handle = tokio::spawn(async move {
        let lock = panic_registry.lock(&panic_session_for_task);
        let _cleanup = UploadSessionLockCleanup::new(&panic_registry, &panic_session_for_task);
        let _guard = lock.lock().await;
        tokio::task::yield_now().await;
        panic!("intentional session lock holder panic");
    });
    match panic_handle.await {
        Err(err) if err.is_panic() => {}
        other => panic!("expected panic from lock holder task, got {other:?}"),
    }

    let lock = registry.lock(&panic_session);
    let _cleanup = UploadSessionLockCleanup::new(&registry, &panic_session);
    let guard = tokio::time::timeout(Duration::from_secs(2), lock.lock())
        .await
        .expect("session lock should remain acquirable after holder panic");
    drop(guard);
    Ok(())
}
