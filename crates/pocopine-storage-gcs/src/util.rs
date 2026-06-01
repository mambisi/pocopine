use google_cloud_gax::error::rpc::Code;
use pocopine_storage::backend_common::ensure_open;
use pocopine_storage::{StorageError, StorageResult, UploadSession, UploadSessionStatus};

pub(crate) fn ensure_completable(session: &UploadSession) -> StorageResult<()> {
    if session.status == UploadSessionStatus::Completing {
        Ok(())
    } else {
        ensure_open(session)
    }
}

pub(crate) fn non_empty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

pub(crate) fn positive_generation(generation: i64) -> Option<i64> {
    if generation > 0 {
        Some(generation)
    } else {
        None
    }
}

pub(crate) fn usize_from_u64(value: u64) -> StorageResult<usize> {
    usize::try_from(value)
        .map_err(|_| StorageError::policy_rejected("upload byte count exceeds host capacity"))
}

pub(crate) fn map_session_write_error(error: StorageError) -> StorageError {
    match error {
        StorageError::PolicyRejected { .. } => {
            StorageError::conflict("GCS upload session changed concurrently")
        }
        other => other,
    }
}

pub(crate) fn is_gcs_precondition_failed(err: &google_cloud_storage::Error) -> bool {
    err.http_status_code().is_some_and(|status| status == 412)
        || err
            .status()
            .is_some_and(|status| status.code == Code::FailedPrecondition)
}

pub(crate) fn is_gcs_not_found(err: &google_cloud_storage::Error) -> bool {
    err.http_status_code().is_some_and(|status| status == 404)
        || err
            .status()
            .is_some_and(|status| status.code == Code::NotFound)
}

pub(crate) fn gcs_error(operation: &'static str, err: impl std::fmt::Display) -> StorageError {
    tracing::error!(
        target: "pocopine.log",
        event_name = "pocopine.storage.gcs_error",
        operation,
        error = %err,
    );
    StorageError::backend(format!("GCS {operation}: {err}"))
}
