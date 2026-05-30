//! Provider-write amplification: the legacy staged-object rewrite vs the native
//! upload strategies introduced for #176.
//!
//! This models the bytes each strategy pushes to the object store for an upload
//! delivered as a sequence of `PATCH` chunks, and moves those bytes through an
//! in-memory sink so wall-clock time tracks the I/O volume. It is a Docker-free
//! proxy for object-store write traffic, not a network benchmark — but the byte
//! counts it reports are exact.
//!
//! Strategies:
//! - `gcs_legacy_rewrite`: the OLD path (both S3 and GCS) — every `PATCH`
//!   re-uploads the whole staged object, and completion writes it once more.
//!   O(n^2) provider bytes.
//! - `s3_native_multipart` (AWS baseline): coalesce to >=5 MiB parts; each byte
//!   is uploaded exactly once via `UploadPart`. O(n).
//! - `gcs_native_compose` (current GCS): each chunk is one component written
//!   once; the final object is a server-side compose (no client re-upload). O(n).
//!
//! Run: `cargo bench -p pocopine-storage --bench amplification`.

#![cfg(not(target_arch = "wasm32"))]

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

const CHUNK: usize = 1024 * 1024; // 1 MiB PATCH chunks
const S3_PART: usize = 5 * 1024 * 1024; // S3 minimum part size
const MIB: f64 = 1024.0 * 1024.0;

/// Counts bytes written and folds them so the compiler cannot elide the copy.
#[derive(Default)]
struct Sink {
    written: u64,
    fold: u64,
}

impl Sink {
    #[inline]
    fn write(&mut self, buf: &[u8]) {
        self.written += buf.len() as u64;
        let mut acc = 0u64;
        for &byte in buf {
            acc = acc.wrapping_add(byte as u64);
        }
        self.fold = self.fold.wrapping_add(acc);
    }
}

/// Legacy: re-upload the whole staged object on every append, then once more on
/// completion. Provider write bytes grow quadratically with object size.
fn gcs_legacy_rewrite(total: usize, chunk: usize) -> Sink {
    let mut sink = Sink::default();
    let mut staged: Vec<u8> = Vec::with_capacity(total);
    let mut offset = 0;
    while offset < total {
        let n = chunk.min(total - offset);
        staged.resize(staged.len() + n, 0xA5);
        sink.write(&staged); // put_staged rewrites the entire object
        offset += n;
    }
    sink.write(&staged); // final completed-object write
    sink
}

/// S3 native multipart: coalesce sub-part chunks into >=5 MiB parts, flushing
/// each part's bytes to the provider exactly once via `UploadPart`.
///
/// Only the unflushed remainder is buffered (bounded by one part), tracked by
/// length with no front-shifting, so the model reflects the real cost: every
/// payload byte is written once. (The real backend additionally rewrites a small
/// base64 tail in the session metadata per append; that churn is bounded by the
/// part size and disappears once the client sends part-sized chunks, so it is
/// not modeled here.)
fn s3_native_multipart(total: usize, chunk: usize) -> Sink {
    let mut sink = Sink::default();
    let part = vec![0xA5u8; S3_PART];
    let mut pending = 0usize;
    let mut offset = 0;
    while offset < total {
        let n = chunk.min(total - offset);
        pending += n;
        while pending >= S3_PART {
            sink.write(&part); // UploadPart: one full part, written once
            pending -= S3_PART;
        }
        offset += n;
    }
    if pending > 0 {
        sink.write(&part[..pending]); // final part
    }
    sink
}

/// GCS native compose: each chunk is one component, written once.
fn gcs_native_compose(total: usize, chunk: usize) -> Sink {
    let mut sink = Sink::default();
    let block = vec![0xA5u8; chunk];
    let mut offset = 0;
    while offset < total {
        let n = chunk.min(total - offset);
        sink.write(&block[..n]); // component write
        offset += n;
    }
    sink
}

fn amplification(c: &mut Criterion) {
    let totals = [4 * 1024 * 1024usize, 16 * 1024 * 1024, 64 * 1024 * 1024];

    // Exact provider-write-byte report (the headline numbers).
    eprintln!(
        "\n=== provider write volume, {} KiB PATCH chunks ===",
        CHUNK / 1024
    );
    eprintln!(
        "{:>10}  {:>16}  {:>16}  {:>16}  {:>10}",
        "upload", "gcs_legacy O(n^2)", "s3_multipart O(n)", "gcs_compose O(n)", "legacy x"
    );
    for &total in &totals {
        let legacy = gcs_legacy_rewrite(total, CHUNK).written;
        let s3 = s3_native_multipart(total, CHUNK).written;
        let gcs = gcs_native_compose(total, CHUNK).written;
        eprintln!(
            "{:>7} MiB  {:>12.1} MiB  {:>12.1} MiB  {:>12.1} MiB  {:>9.1}x",
            total / 1024 / 1024,
            legacy as f64 / MIB,
            s3 as f64 / MIB,
            gcs as f64 / MIB,
            legacy as f64 / gcs as f64,
        );
    }
    eprintln!();

    let mut group = c.benchmark_group("provider_write_cost");
    for &total in &totals {
        let mib = total / 1024 / 1024;
        group.throughput(Throughput::Bytes(total as u64));
        group.bench_with_input(
            BenchmarkId::new("gcs_legacy_rewrite", mib),
            &total,
            |b, &t| b.iter(|| black_box(gcs_legacy_rewrite(t, CHUNK).fold)),
        );
        group.bench_with_input(
            BenchmarkId::new("s3_native_multipart", mib),
            &total,
            |b, &t| b.iter(|| black_box(s3_native_multipart(t, CHUNK).fold)),
        );
        group.bench_with_input(
            BenchmarkId::new("gcs_native_compose", mib),
            &total,
            |b, &t| b.iter(|| black_box(gcs_native_compose(t, CHUNK).fold)),
        );
    }
    group.finish();
}

criterion_group!(benches, amplification);
criterion_main!(benches);
