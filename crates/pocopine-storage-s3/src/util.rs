use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::operation::get_object::GetObjectError;
use aws_sdk_s3::operation::head_object::HeadObjectError;
use aws_sdk_s3::operation::put_object::PutObjectError;
use pocopine_storage::backend_common::ensure_open;
use pocopine_storage::{StorageError, StorageResult, UploadSession, UploadSessionStatus};

pub(crate) fn ensure_completable(session: &UploadSession) -> StorageResult<()> {
    if session.status == UploadSessionStatus::Completing {
        Ok(())
    } else {
        ensure_open(session)
    }
}

pub(crate) fn normalize_etag(etag: String) -> String {
    etag.trim_matches('"').to_string()
}

pub(crate) fn is_get_object_not_found(err: &SdkError<GetObjectError>) -> bool {
    err.as_service_error()
        .is_some_and(GetObjectError::is_no_such_key)
}

pub(crate) fn is_head_object_not_found(err: &SdkError<HeadObjectError>) -> bool {
    err.as_service_error()
        .is_some_and(HeadObjectError::is_not_found)
}

pub(crate) fn is_put_precondition_failed(err: &SdkError<PutObjectError>) -> bool {
    err.as_service_error()
        .and_then(|err| err.meta().code())
        .is_some_and(|code| code == "PreconditionFailed")
}

/// True when an S3 multipart operation failed because the upload id is gone
/// (already completed or aborted). Used to make completion/abort idempotent.
pub(crate) fn is_no_such_upload<E>(err: &SdkError<E>) -> bool
where
    E: ProvideErrorMetadata,
{
    err.as_service_error()
        .and_then(ProvideErrorMetadata::code)
        .is_some_and(|code| code == "NoSuchUpload")
}

pub(crate) fn s3_error(operation: &'static str, err: impl std::fmt::Display) -> StorageError {
    tracing::error!(
        target: "pocopine.log",
        event_name = "pocopine.storage.s3_error",
        operation,
        error = %err,
    );
    StorageError::backend(format!("S3 {operation}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::normalize_etag;

    #[test]
    fn etag_quotes_are_removed() {
        assert_eq!(normalize_etag("\"abc123\"".to_string()), "abc123");
    }
}
