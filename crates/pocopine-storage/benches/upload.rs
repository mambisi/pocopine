//! Throughput benchmark for the upload hot path (initiate → N appends →
//! complete) against the in-memory backend.
//!
//! This isolates the framework's per-chunk overhead (session bookkeeping,
//! per-session locking, offset checks, append handling) from provider network
//! I/O. The headline #176 win — bounded provider-write amplification for the S3
//! and GCS native backends — is validated separately by their integration
//! tests (part/component counts stay O(object_size / part_size), not O(n²)).
//!
//! Run with: `cargo bench -p pocopine-storage --bench upload`.

// The criterion + tokio bench harness is host-only. Gating it off wasm32 would
// leave this `harness = false` bench binary with no `main` (E0601), so the body
// lives in a host-only module and wasm32 gets an empty `main` instead — keeps
// `clippy --all-targets --target wasm32-unknown-unknown` green.
#[cfg(not(target_arch = "wasm32"))]
mod bench {
    use bytes::Bytes;
    use criterion::{BenchmarkId, Criterion, Throughput, criterion_group};
    use pocopine_storage::{
        CompleteUpload, InitiateUpload, MemoryStorageBackend, SafeObjectKey, StorageBackend,
        StorageContext, StorageKey, UploadPolicy, UploadStrategy,
    };
    use tokio::runtime::Builder;

    const TOTAL: usize = 16 * 1024 * 1024;

    async fn run_upload(total: usize, chunk: usize) {
        let backend = MemoryStorageBackend::new();
        let ctx = StorageContext::system("bench");
        let policy = UploadPolicy::new("memory")
            .expect("policy")
            .max_bytes(total as u64);
        let session = backend
            .initiate_upload(
                &ctx,
                InitiateUpload {
                    scope: "files".to_string(),
                    storage_key: StorageKey::new(
                        SafeObjectKey::parse("files/bench.bin").expect("key"),
                    ),
                    file_name: "bench.bin".to_string(),
                    size: Some(total as u64),
                    content_type: Some("application/octet-stream".to_string()),
                    metadata: Default::default(),
                    requested_strategy: UploadStrategy::Auto,
                    policy,
                },
            )
            .await
            .expect("initiate");

        let block = Bytes::from(vec![0u8; chunk]);
        let mut offset = 0usize;
        while offset < total {
            let len = chunk.min(total - offset);
            backend
                .append_upload_bytes(&ctx, session.id.clone(), offset as u64, block.slice(0..len))
                .await
                .expect("append");
            offset += len;
        }
        backend
            .complete_upload(
                &ctx,
                CompleteUpload {
                    session: session.id,
                    checksum: None,
                },
            )
            .await
            .expect("complete");
    }

    fn upload_throughput(c: &mut Criterion) {
        let runtime = Builder::new_current_thread().build().expect("runtime");
        let mut group = c.benchmark_group("memory_upload_16MiB");
        group.throughput(Throughput::Bytes(TOTAL as u64));
        for &chunk in &[64 * 1024usize, 256 * 1024, 1024 * 1024, 4 * 1024 * 1024] {
            group.bench_with_input(BenchmarkId::from_parameter(chunk), &chunk, |b, &chunk| {
                b.to_async(&runtime).iter(|| run_upload(TOTAL, chunk));
            });
        }
        group.finish();
    }

    criterion_group!(benches, upload_throughput);
}

#[cfg(not(target_arch = "wasm32"))]
criterion::criterion_main!(bench::benches);

#[cfg(target_arch = "wasm32")]
fn main() {}
