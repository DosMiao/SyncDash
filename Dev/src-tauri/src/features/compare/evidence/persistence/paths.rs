//! Typed paths and inventory names for the Compare-result store.

use syncdash::foundation::path::{RootRelativeDir, RootRelativePath};

use super::artifact_validation::validate_result_id;
use super::error::invalid_data;
use super::{INDEX_FILE_NAME, LOCK_FILE_NAME, RESULT_DIRECTORY_NAME, RESULT_FILE_SUFFIX};

pub(super) fn parse_result_file_name(name: &str) -> Option<&str> {
    name.strip_suffix(RESULT_FILE_SUFFIX)
        .filter(|result_id| validate_result_id(result_id).is_ok())
}

pub(super) fn result_file_name(result_id: &str) -> String {
    format!("{result_id}{RESULT_FILE_SUFFIX}")
}

pub(super) fn result_path(result_id: &str) -> std::io::Result<RootRelativePath> {
    validate_result_id(result_id)?;
    RootRelativePath::new(format!(
        "{RESULT_DIRECTORY_NAME}/{}",
        result_file_name(result_id)
    ))
    .map_err(|error| invalid_data(error.to_string()))
}

pub(super) fn root_directory() -> RootRelativeDir {
    RootRelativeDir::new("").expect("the empty relative directory names the root")
}

pub(super) fn result_directory() -> RootRelativeDir {
    RootRelativeDir::new(RESULT_DIRECTORY_NAME).expect("constant result directory is valid")
}

pub(super) fn index_path() -> RootRelativePath {
    RootRelativePath::new(INDEX_FILE_NAME).expect("constant index name is valid")
}

pub(super) fn lock_path() -> RootRelativePath {
    RootRelativePath::new(LOCK_FILE_NAME).expect("constant lock name is valid")
}
