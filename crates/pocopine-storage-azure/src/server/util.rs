use azure_core::error::ErrorKind;
use azure_core::http::StatusCode;
use azure_storage_blob::models::StorageErrorCode;
use pocopine_storage::backend_common::ensure_open;
use pocopine_storage::{StorageError, StorageResult, UploadSession, UploadSessionStatus};

pub(crate) fn ensure_completable(session: &UploadSession) -> StorageResult<()> {
    if session.status == UploadSessionStatus::Completing {
        Ok(())
    } else {
        ensure_open(session)
    }
}

pub(crate) fn usize_from_u64(value: u64) -> StorageResult<usize> {
    usize::try_from(value)
        .map_err(|_| StorageError::policy_rejected("upload byte count exceeds host capacity"))
}

pub(crate) fn map_session_write_error(error: StorageError) -> StorageError {
    match error {
        StorageError::Conflict { .. } | StorageError::PolicyRejected { .. } => {
            StorageError::conflict("Azure upload session changed concurrently")
        }
        other => other,
    }
}

pub(crate) fn is_azure_not_found(err: &azure_core::Error) -> bool {
    err.http_status() == Some(StatusCode::NotFound)
        || matches!(
            azure_error_code(err),
            Some(code)
                if code == StorageErrorCode::BlobNotFound.as_ref()
                    || code == StorageErrorCode::ContainerNotFound.as_ref()
                    || code == StorageErrorCode::ResourceNotFound.as_ref()
        )
}

pub(crate) fn is_azure_already_exists(err: &azure_core::Error) -> bool {
    err.http_status() == Some(StatusCode::Conflict)
        && matches!(
            azure_error_code(err),
            Some(code)
                if code == StorageErrorCode::BlobAlreadyExists.as_ref()
                    || code == StorageErrorCode::ContainerAlreadyExists.as_ref()
                    || code == StorageErrorCode::ResourceAlreadyExists.as_ref()
        )
}

pub(crate) fn is_azure_precondition_failed(err: &azure_core::Error) -> bool {
    err.http_status() == Some(StatusCode::PreconditionFailed)
        || matches!(
            azure_error_code(err),
            Some(code)
                if code == StorageErrorCode::ConditionNotMet.as_ref()
                    || code == StorageErrorCode::TargetConditionNotMet.as_ref()
                    || code == StorageErrorCode::SourceConditionNotMet.as_ref()
        )
}

pub(crate) fn azure_error(operation: &'static str, err: impl std::fmt::Display) -> StorageError {
    tracing::error!(
        target: "pocopine.log",
        event_name = "pocopine.storage.azure_error",
        operation,
        error = %err,
    );
    StorageError::backend(format!("Azure Blob Storage {operation}: {err}"))
}

fn azure_error_code(err: &azure_core::Error) -> Option<&str> {
    match err.kind() {
        ErrorKind::HttpResponse {
            error_code: Some(error_code),
            ..
        } => Some(error_code.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use azure_core::error::ErrorKind;
    use azure_core::http::StatusCode;

    use super::*;

    fn azure_http_error(status: StatusCode, error_code: Option<&str>) -> azure_core::Error {
        ErrorKind::HttpResponse {
            status,
            error_code: error_code.map(str::to_string),
            raw_response: None,
        }
        .into_error()
    }

    #[test]
    fn classifies_not_found_errors() {
        let missing = azure_http_error(StatusCode::NotFound, Some("BlobNotFound"));
        assert!(is_azure_not_found(&missing));
        assert!(!is_azure_precondition_failed(&missing));
    }

    #[test]
    fn classifies_already_exists_errors() {
        let exists = azure_http_error(StatusCode::Conflict, Some("BlobAlreadyExists"));
        assert!(is_azure_already_exists(&exists));
        assert!(!is_azure_not_found(&exists));
    }

    #[test]
    fn classifies_precondition_errors() {
        let failed = azure_http_error(StatusCode::PreconditionFailed, Some("ConditionNotMet"));
        assert!(is_azure_precondition_failed(&failed));
        assert!(!is_azure_already_exists(&failed));
    }
}
