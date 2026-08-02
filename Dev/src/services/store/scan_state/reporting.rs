//! Best-effort failure policy at the persistence boundary.

use std::path::Path;

pub(crate) fn report_read<T>(path: &Path, result: std::io::Result<T>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            crate::log_warn!(
                "scan-state",
                "scan state read failed for {}: {error}; this run will rebuild that acceleration data",
                path.display()
            );
            None
        }
    }
}

pub(crate) fn report_write(
    path: &Path,
    result: std::io::Result<bool>,
) -> super::super::StateWriteStatus {
    use super::super::StateWriteStatus;

    match result {
        Ok(true) => StateWriteStatus::Written,
        Ok(false) => {
            crate::log_warn!(
                "scan-state",
                "scan state at {} was written by a newer SyncDash version and was left unchanged",
                path.display()
            );
            StateWriteStatus::PreservedNewerVersion
        }
        Err(error) => {
            crate::log_warn!(
                "scan-state",
                "scan state write failed for {}: {error}; sync results remain valid, but later scans may need to reread files",
                path.display()
            );
            StateWriteStatus::Failed
        }
    }
}
