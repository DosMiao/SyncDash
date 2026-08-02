//! L1 observability: one event stream and several sinks.
//!
//! `progress` owns the event vocabulary plus the cancel/pause control plane and the
//! process-global sink registry (the registry stores `ProgressSink`, so it lives with
//! the trait — housing it in `logging` made the two modules mutually dependent).
//! `logging` supplies the concrete sinks and the `log_*!` macros for code that cannot
//! reach a `RunCtx`. Durable run history belongs to L3 orchestration in `run::history`, where
//! job identity and run lifecycle are already owned.

pub mod logging;
pub mod progress;
