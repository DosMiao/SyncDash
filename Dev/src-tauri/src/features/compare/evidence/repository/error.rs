//! Repository-facing storage error projection.

use super::super::model::error::CompareResultRepositoryError;

pub(super) fn storage_error(context: &str, error: std::io::Error) -> CompareResultRepositoryError {
    CompareResultRepositoryError::Storage(format!("{context}: {error}"))
}
