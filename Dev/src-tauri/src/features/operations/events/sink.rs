use std::sync::Arc;

use syncdash::model::event::ProgressEvent;
use syncdash::obs::progress::{ProgressSink, RunCtl, RunCtx};
use tauri::Emitter;

use super::model::RunEventAudience;
use super::repository::RunEventRepository;
use super::throttle::ProgressThrottle;

struct TauriSink {
    app: tauri::AppHandle,
    events: Arc<RunEventRepository>,
    run_id: u64,
    audience: RunEventAudience,
    throttle: ProgressThrottle,
}

impl ProgressSink for TauriSink {
    fn emit(&self, ev: ProgressEvent) {
        if let ProgressEvent::Progress { phase, ts_ms, .. } = &ev {
            if !self.throttle.allows(*phase, *ts_ms) {
                return;
            }
        }
        let event = self.events.record(self.run_id, self.audience.purpose(), ev);
        let _ = self
            .app
            .emit_to(self.audience.window_label(), "run-progress", event);
    }
}

pub(crate) fn make_ctx(
    app: &tauri::AppHandle,
    events: Arc<RunEventRepository>,
    run_id: u64,
    ctl: Arc<RunCtl>,
    audience: RunEventAudience,
) -> RunCtx {
    RunCtx::new(
        ctl,
        Arc::new(TauriSink {
            app: app.clone(),
            events,
            run_id,
            audience,
            throttle: ProgressThrottle::default(),
        }),
    )
}
