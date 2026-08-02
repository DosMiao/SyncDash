use crate::contracts::events::RunEventPurposeDto;
use crate::window::{MAIN_WINDOW_LABEL, PROGRESS_WINDOW_LABEL};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunEventAudience {
    Compare,
    Apply,
}

impl RunEventAudience {
    pub(super) const fn purpose(self) -> RunEventPurposeDto {
        match self {
            Self::Compare => RunEventPurposeDto::Compare,
            Self::Apply => RunEventPurposeDto::Apply,
        }
    }

    pub(super) const fn window_label(self) -> &'static str {
        match self {
            Self::Compare => MAIN_WINDOW_LABEL,
            Self::Apply => PROGRESS_WINDOW_LABEL,
        }
    }
}
