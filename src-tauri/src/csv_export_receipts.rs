//! One-use receipts for revealing a CSV file created by the backend export command.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const RECEIPT_LIFETIME: Duration = Duration::from_secs(30 * 60);

struct CsvExportReceipt {
    path: PathBuf,
    issued_at: Instant,
}

#[derive(Default)]
pub(crate) struct CsvExportReceiptRepository {
    receipts: Mutex<HashMap<String, CsvExportReceipt>>,
}

impl CsvExportReceiptRepository {
    pub(crate) fn issue(&self, path: PathBuf) -> Result<String, String> {
        let receipt_id =
            crate::authority_token::random_hex::<16>("Cannot create an export receipt")?;
        let now = Instant::now();
        let mut receipts = self.receipts.lock().unwrap();
        receipts.retain(|_, receipt| now.duration_since(receipt.issued_at) <= RECEIPT_LIFETIME);
        receipts.insert(
            receipt_id.clone(),
            CsvExportReceipt {
                path,
                issued_at: now,
            },
        );
        Ok(receipt_id)
    }

    pub(crate) fn revoke(&self, receipt_id: &str) {
        self.receipts.lock().unwrap().remove(receipt_id);
    }

    pub(crate) fn consume_with<T>(
        &self,
        receipt_id: &str,
        use_path: impl FnOnce(&std::path::Path) -> Result<T, String>,
    ) -> Result<T, String> {
        let now = Instant::now();
        let mut receipts = self.receipts.lock().unwrap();
        receipts.retain(|_, receipt| now.duration_since(receipt.issued_at) <= RECEIPT_LIFETIME);
        let receipt = receipts
            .get(receipt_id)
            .ok_or_else(|| "The CSV export receipt expired — export the file again".to_string())?;
        let result = use_path(&receipt.path)?;
        receipts.remove(receipt_id);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_resolves_the_backend_owned_path_exactly_once() {
        let repository = CsvExportReceiptRepository::default();
        let path = PathBuf::from("/tmp/syncdash-export.csv");
        let receipt = repository.issue(path.clone()).unwrap();
        assert_eq!(
            repository
                .consume_with(&receipt, |resolved| Ok(resolved.to_path_buf()))
                .unwrap(),
            path
        );
        assert!(repository.consume_with(&receipt, |_| Ok(())).is_err());
    }

    #[test]
    fn failed_use_preserves_the_receipt_for_a_retry() {
        let repository = CsvExportReceiptRepository::default();
        let receipt = repository.issue(PathBuf::from("/tmp/retry.csv")).unwrap();
        assert!(repository
            .consume_with::<()>(&receipt, |_| Err("file manager unavailable".into()))
            .is_err());
        assert!(repository.consume_with(&receipt, |_| Ok(())).is_ok());
    }
}
