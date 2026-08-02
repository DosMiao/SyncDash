use serde::Serialize;
use syncdash::model::event::ProgressEvent;

use crate::window::{MAIN_WINDOW_LABEL, PROGRESS_WINDOW_LABEL};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunEventAudience {
    Compare,
    Apply,
}

impl RunEventAudience {
    pub(super) const fn purpose(self) -> &'static str {
        match self {
            Self::Compare => "compare",
            Self::Apply => "apply",
        }
    }

    pub(super) const fn window_label(self) -> &'static str {
        match self {
            Self::Compare => MAIN_WINDOW_LABEL,
            Self::Apply => PROGRESS_WINDOW_LABEL,
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct RunEvent {
    pub(crate) sequence: u64,
    pub(crate) run_id: u64,
    pub(crate) purpose: &'static str,
    #[serde(flatten)]
    pub(crate) ev: ProgressEvent,
}
