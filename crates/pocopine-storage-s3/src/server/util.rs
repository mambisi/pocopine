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

/// True when an S3 operation failed its `If-None-Match: *` precondition because
/// the destination key already exists (HTTP 412).
pub(crate) fn is_precondition_failed<E>(err: &SdkError<E>) -> bool
where
    E: ProvideErrorMetadata,
{
    err.as_service_error()
        .and_then(ProvideErrorMetadata::code)
        .is_some_and(|code| code == "PreconditionFailed")
}

pub(crate) fn is_put_precondition_failed(err: &SdkError<PutObjectError>) -> bool {
    put_error_parts_indicate_precondition_failed(put_error_code(err), put_error_status(err))
}

pub(crate) fn is_put_conditional_header_unsupported(err: &SdkError<PutObjectError>) -> bool {
    put_error_parts_indicate_conditional_header_unsupported(
        put_error_code(err),
        put_error_message(err),
        put_error_status(err),
    )
}

pub(crate) fn put_error_code(err: &SdkError<PutObjectError>) -> Option<&str> {
    err.code()
}

pub(crate) fn put_error_message(err: &SdkError<PutObjectError>) -> Option<&str> {
    err.message()
}

pub(crate) fn put_error_status(err: &SdkError<PutObjectError>) -> Option<u16> {
    err.raw_response()
        .map(|response| response.status().as_u16())
}

fn put_error_parts_indicate_precondition_failed(code: Option<&str>, status: Option<u16>) -> bool {
    code.is_some_and(|code| code == "PreconditionFailed") || status == Some(412)
}

fn put_error_parts_indicate_conditional_header_unsupported(
    code: Option<&str>,
    message: Option<&str>,
    status: Option<u16>,
) -> bool {
    if put_error_parts_indicate_precondition_failed(code, status) {
        return false;
    }

    let status_allows_header_rejection = matches!(status, Some(400) | Some(501) | None);
    if !status_allows_header_rejection {
        return false;
    }

    let code = code.unwrap_or_default().to_ascii_lowercase();
    let message = message.unwrap_or_default().to_ascii_lowercase();
    if status == Some(501) && code == "notimplemented" {
        return true;
    }

    let text = format!("{code} {message}");
    let mentions_conditional_header = text.contains("if-none-match")
        || text.contains("if none match")
        || text.contains("if_none_match")
        || text.contains("conditional");
    let rejects_header = code == "notimplemented"
        || code == "not_implemented"
        || code == "notsupported"
        || code == "unsupportedheader"
        || text.contains("unsupported")
        || text.contains("not supported")
        || text.contains("not implement")
        || text.contains("not allowed");

    mentions_conditional_header && rejects_header
}

pub(crate) fn s3_put_error(operation: &'static str, err: SdkError<PutObjectError>) -> StorageError {
    let status = put_error_status(&err);
    let code = put_error_code(&err).map(str::to_owned);
    let message = put_error_message(&err).map(str::to_owned);
    tracing::error!(
        target: "pocopine.log",
        event_name = "pocopine.storage.s3_error",
        operation,
        status = ?status,
        error_code = ?code,
        error_message = ?message,
        error = %err,
        debug = ?err,
    );
    match (status, code, message) {
        (Some(status), Some(code), Some(message)) => {
            StorageError::backend(format!("S3 {operation}: HTTP {status} {code}: {message}"))
        }
        (Some(status), Some(code), None) => {
            StorageError::backend(format!("S3 {operation}: HTTP {status} {code}"))
        }
        (Some(status), None, Some(message)) => {
            StorageError::backend(format!("S3 {operation}: HTTP {status}: {message}"))
        }
        (None, Some(code), Some(message)) => {
            StorageError::backend(format!("S3 {operation}: {code}: {message}"))
        }
        _ => StorageError::backend(format!("S3 {operation}: {err}")),
    }
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
    use super::{
        normalize_etag, put_error_parts_indicate_conditional_header_unsupported,
        put_error_parts_indicate_precondition_failed,
    };

    #[test]
    fn etag_quotes_are_removed() {
        assert_eq!(normalize_etag("\"abc123\"".to_string()), "abc123");
    }

    #[test]
    fn precondition_failure_matches_code_or_status() {
        assert!(put_error_parts_indicate_precondition_failed(
            Some("PreconditionFailed"),
            None
        ));
        assert!(put_error_parts_indicate_precondition_failed(
            Some("InvalidRequest"),
            Some(412)
        ));
    }

    #[test]
    fn conditional_header_unsupported_matches_provider_rejection() {
        assert!(put_error_parts_indicate_conditional_header_unsupported(
            Some("NotImplemented"),
            Some("If-None-Match header is not implemented"),
            Some(501),
        ));
        assert!(put_error_parts_indicate_conditional_header_unsupported(
            Some("InvalidRequest"),
            Some("Unsupported header 'If-None-Match'"),
            Some(400),
        ));
        assert!(put_error_parts_indicate_conditional_header_unsupported(
            Some("NotImplemented"),
            Some("A header you provided implies functionality that is not implemented"),
            Some(501),
        ));
    }

    #[test]
    fn conditional_header_unsupported_does_not_mask_other_put_errors() {
        assert!(!put_error_parts_indicate_conditional_header_unsupported(
            Some("PreconditionFailed"),
            Some("At least one precondition failed"),
            Some(412),
        ));
        assert!(!put_error_parts_indicate_conditional_header_unsupported(
            Some("AccessDenied"),
            Some("Access denied"),
            Some(403),
        ));
        assert!(!put_error_parts_indicate_conditional_header_unsupported(
            Some("SignatureDoesNotMatch"),
            Some("The request signature we calculated does not match"),
            Some(403),
        ));
        assert!(!put_error_parts_indicate_conditional_header_unsupported(
            Some("InvalidRequest"),
            Some("Malformed request body"),
            Some(400),
        ));
    }
}
