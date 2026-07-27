//! L1 observability: one event stream, several sinks, and the persistent run history.
//!
//! `progress` owns the event vocabulary plus the cancel/pause control plane and the
//! process-global sink registry (the registry stores `ProgressSink`, so it lives with
//! the trait — housing it in `logging` made the two modules mutually dependent).
//! `logging` supplies the concrete sinks and the `log_*!` macros for code that cannot
//! reach a `RunCtx`. `runlog` owns the on-disk run history and its retention policy.

pub mod logging;
pub mod progress;
pub mod runlog;
