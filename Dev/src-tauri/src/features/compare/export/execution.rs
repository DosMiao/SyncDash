//! Writing a Compare result to a chosen destination, as one transaction.
//!
//! The receipt is what later authorizes revealing the file in the OS file manager, so it must
//! exist before the write starts and must not survive a write that failed. Issue → stage → seal →
//! commit → keep, with a revoke on every failure path including a panicking worker: a receipt for
//! a file that was never written is an authority to reveal something that is not there, or worse,
//! whatever was there before.
//!
//! The write is staged and published by rename, so an interrupted export leaves the previous file
//! intact rather than a truncated one.

use std::path::PathBuf;
use std::sync::Arc;

use crate::contracts::compare::{CompareIdentity, CsvExportDto, CsvRowPresentationDto};
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::compare::export::filename::default_export_filename;
use crate::features::compare::export::receipt::CsvExportReceiptRepository;
use crate::features::compare::export::render::write_compare_csv;

/// The parts of a retained Compare result an export needs, taken while the repository lock is
/// held so the write itself never holds it.
pub(crate) struct ExportSource {
    pub(crate) default_filename: String,
    header: syncdash::model::plan::PlanHeader,
    operations: Vec<syncdash::model::plan::Op>,
    metadata: Vec<Option<syncdash::pipeline::compare::evidence::RowMeta>>,
}

/// Read the retained result, or explain that it is gone.
pub(crate) fn prepare(
    results: &CompareResultRepository,
    compare_identity: &CompareIdentity,
) -> Result<ExportSource, String> {
    let retained = results
        .require_exact(compare_identity)
        .map_err(|error| error.to_string())?;
    let default_filename = default_export_filename(
        &retained.owner().job_name,
        retained.identity().compare_run_id,
        syncdash::foundation::time::now_ms(),
    )?;
    Ok(ExportSource {
        default_filename,
        header: retained.plan_header().clone(),
        operations: retained.plan_operations().to_vec(),
        metadata: retained.plan_metadata().to_vec(),
    })
}

/// Issue a receipt, write the CSV, and keep the receipt only if the write succeeded.
pub(crate) async fn write_to(
    receipts: Arc<CsvExportReceiptRepository>,
    source: ExportSource,
    destination: PathBuf,
    rows: Vec<CsvRowPresentationDto>,
) -> Result<CsvExportDto, String> {
    let receipt_id = receipts.issue(destination.clone())?;
    let write_result = tauri::async_runtime::spawn_blocking({
        let destination = destination.clone();
        move || {
            let mut staged = syncdash::fs::staged::Staged::create(&destination)
                .map_err(|error| format!("{}: {error}", destination.display()))?;
            let row_count = write_compare_csv(
                &mut staged,
                &source.header,
                &source.operations,
                &source.metadata,
                &rows,
            )?;
            staged
                .seal(true)
                .and_then(|()| staged.commit())
                .map_err(|error| format!("{}: {error}", destination.display()))?;
            Ok::<usize, String>(row_count)
        }
    })
    .await;
    let row_count = match write_result {
        Ok(Ok(row_count)) => row_count,
        Ok(Err(error)) => {
            receipts.revoke(&receipt_id);
            return Err(error);
        }
        Err(error) => {
            receipts.revoke(&receipt_id);
            return Err(format!("The CSV export worker failed: {error}"));
        }
    };
    Ok(CsvExportDto::Exported {
        row_count,
        display_path: destination.to_string_lossy().into_owned(),
        receipt_id,
    })
}
