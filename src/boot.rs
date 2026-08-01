//! Process startup: the three things every shell does before it may call the library.
//!
//! Both shells opened with the same sequence — size the worker pool, load settings, install a
//! progress sink whose guard must outlive every later call — and both carried the same comment
//! warning that writing `let _ = install(...)` drops the guard on the spot and silently unhooks
//! the sink. Two copies of a startup order is the kind of duplication that goes wrong quietly:
//! the CLI grew `mirror_stderr` and the desktop grew log pruning, and neither knew about the other.
//!
//! L3: it reaches down into `store::settings` and `obs::progress`, and both L4 shells call it.

use std::sync::Arc;

use crate::obs::progress::{ProgressSink, SinkGuard};
use crate::store::settings::AppSettings;

/// What `init` hands back. **Hold it for the life of the process**: dropping it uninstalls the
/// progress sink, after which the library's diagnostics go nowhere.
pub struct Session {
    pub settings: AppSettings,
    _sink: Option<SinkGuard>,
}

/// Size the pool, load settings, install the sink the shell chooses for them.
///
/// The sink is a closure rather than a value because it depends on the settings this function is
/// loading — the CLI installs one only when `mirror_stderr` is set, the desktop always opens the
/// app log at the resolved log directory.
pub fn init(sink_for: impl FnOnce(&AppSettings) -> Option<Arc<dyn ProgressSink>>) -> Session {
    init_worker_pool();
    let (settings, diagnostic) = crate::store::settings::load_with_diagnostic();
    let guard = sink_for(&settings).map(crate::obs::progress::install);
    if let Some(message) = diagnostic {
        crate::obs::logging::emit(
            crate::model::event::LogLevel::Warn,
            "settings",
            message,
        );
    }
    Session { settings, _sink: guard }
}

/// Initialize the global rayon pool (hash worker threads): **lowered priority** —— a standard scan's BLAKE3
/// deliberately runs on every core (get the one-off cost over with) but must not grind the whole machine to a halt;
/// at below-normal priority a full-core hash yields to foreground programs on its own, with no throughput loss on an idle box.
/// `SYNCDASH_SCAN_THREADS=N` caps the thread count further. CLI and desktop each call this once at startup (idempotent).
pub fn init_worker_pool() {
    let mut b = rayon::ThreadPoolBuilder::new().thread_name(|i| format!("sd-hash-{i}"));
    if let Some(threads) = crate::foundation::thread::configured_worker_limit() {
        b = b.num_threads(threads);
    }
    // build_global returns Err when a global pool already exists (repeat call) —— swallowing it is fine
    let _ = b.start_handler(|_| crate::foundation::thread::lower_priority()).build_global();
}
