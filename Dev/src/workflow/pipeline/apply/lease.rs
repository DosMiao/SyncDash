//! Root-lock ownership monitoring across the complete mutation phase.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::obs::progress::{PhaseProgress, RunCtx};

pub(super) struct ApplyLeaseGuard {
    locks: (crate::fs::lock::RootLock, crate::fs::lock::RootLock),
    source_lost: std::sync::Arc<AtomicBool>,
    target_lost: std::sync::Arc<AtomicBool>,
    stop_watcher: std::sync::Arc<AtomicBool>,
    loss_announced: std::sync::Arc<AtomicBool>,
    ctx: RunCtx,
    watcher: Option<std::thread::JoinHandle<()>>,
}

fn lease_loss_error() -> std::io::Error {
    std::io::Error::other("root lock ownership was lost; apply stopped before the next mutation")
}

impl ApplyLeaseGuard {
    pub(super) fn new(
        source: crate::fs::lock::RootLock,
        target: crate::fs::lock::RootLock,
        ctx: &RunCtx,
    ) -> Self {
        let source_lost = source.lease_loss_signal();
        let target_lost = target.lease_loss_signal();
        let stop_watcher = std::sync::Arc::new(AtomicBool::new(false));
        let loss_announced = std::sync::Arc::new(AtomicBool::new(false));
        let watcher_source = source_lost.clone();
        let watcher_target = target_lost.clone();
        let watcher_stop = stop_watcher.clone();
        let watcher_announced = loss_announced.clone();
        let watcher_ctx = ctx.clone();
        let watcher = std::thread::spawn(move || {
            while !watcher_stop.load(Ordering::Acquire) {
                if watcher_source.load(Ordering::Acquire) || watcher_target.load(Ordering::Acquire)
                {
                    watcher_ctx.ctl.set_paused(false);
                    if !watcher_announced.swap(true, Ordering::AcqRel) {
                        watcher_ctx.log(
                            crate::model::event::LogLevel::Error,
                            "apply",
                            "root lock ownership was lost; stopping before further mutations",
                        );
                    }
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });
        Self {
            locks: (source, target),
            source_lost,
            target_lost,
            stop_watcher,
            loss_announced,
            ctx: ctx.clone(),
            watcher: Some(watcher),
        }
    }

    pub(super) fn lost(&self) -> bool {
        self.source_lost.load(Ordering::Acquire) || self.target_lost.load(Ordering::Acquire)
    }

    fn announce_loss(&self) {
        self.ctx.ctl.set_paused(false);
        if !self.loss_announced.swap(true, Ordering::AcqRel) {
            self.ctx.log(
                crate::model::event::LogLevel::Error,
                "apply",
                "root lock ownership was lost; stopping before further mutations",
            );
        }
    }

    pub(super) fn checkpoint(&self, pp: &PhaseProgress<'_>) -> std::io::Result<()> {
        if self.lost() {
            self.announce_loss();
            return Err(lease_loss_error());
        }
        pp.checkpoint()?;
        if self.lost() {
            self.announce_loss();
            return Err(lease_loss_error());
        }
        Ok(())
    }

    pub(super) fn check_before_mutation(&self) -> std::io::Result<()> {
        if !self.locks.0.verify_lease_identity() || !self.locks.1.verify_lease_identity() {
            self.announce_loss();
            Err(lease_loss_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for ApplyLeaseGuard {
    fn drop(&mut self) {
        self.stop_watcher.store(true, Ordering::Release);
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}
