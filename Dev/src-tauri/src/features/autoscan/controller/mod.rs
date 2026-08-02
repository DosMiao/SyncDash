//! AutoScan coordinator split by generation lifecycle and exact ticket transition.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::operations::authorization::store::OperationAuthorizationStore;

use super::model::AutoScanStatusDto;
use super::runtime::{ActiveAutoScan, AutoScanExecutionServices};

mod auto_apply;
mod compare;
mod lifecycle;
mod ownership;
mod shutdown;
pub(super) mod transition;
mod verification;

pub(crate) struct AutoScanController {
    gate: Mutex<()>,
    pub(super) active: Mutex<Option<ActiveAutoScan>>,
    tombstone: Mutex<Option<AutoScanStatusDto>>,
    generations: AtomicU64,
    compare_permits: AtomicU64,
    pub(super) execution: AutoScanExecutionServices,
}

impl AutoScanController {
    pub(crate) fn new(
        results: Arc<CompareResultRepository>,
        authorizations: Arc<OperationAuthorizationStore>,
    ) -> Self {
        Self {
            gate: Mutex::new(()),
            active: Mutex::new(None),
            tombstone: Mutex::new(None),
            generations: AtomicU64::new(0),
            compare_permits: AtomicU64::new(0),
            execution: AutoScanExecutionServices {
                results,
                authorizations,
            },
        }
    }

    #[cfg(test)]
    pub(super) fn set_compare_permit_counter_for_test(&self, value: u64) {
        self.compare_permits
            .store(value, std::sync::atomic::Ordering::Relaxed);
    }
}
