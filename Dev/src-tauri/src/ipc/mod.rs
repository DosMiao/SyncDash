//! Tauri IPC boundary: window-role authorization and page-grouped commands.

pub(crate) mod commands;
pub(crate) mod native;

use crate::window::{MAIN_WINDOW_LABEL, PROGRESS_WINDOW_LABEL};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WindowRole {
    Main,
    Progress,
}

impl WindowRole {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Main => MAIN_WINDOW_LABEL,
            Self::Progress => PROGRESS_WINDOW_LABEL,
        }
    }

    const fn operation_name(self) -> &'static str {
        match self {
            Self::Main => "main workspace",
            Self::Progress => "Apply progress",
        }
    }
}

pub(crate) fn require_window_role(
    window: &tauri::WebviewWindow,
    expected: WindowRole,
) -> Result<(), String> {
    require_window_label(window.label(), expected)
}

fn require_window_label(label: &str, expected: WindowRole) -> Result<(), String> {
    if label == expected.label() {
        Ok(())
    } else {
        Err(format!(
            "This operation is only available from the {} window",
            expected.operation_name()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_accept_only_their_exact_window_label() {
        assert!(require_window_label("main", WindowRole::Main).is_ok());
        assert!(require_window_label("progress", WindowRole::Progress).is_ok());
        assert!(require_window_label("progress", WindowRole::Main).is_err());
        assert!(require_window_label("main", WindowRole::Progress).is_err());
        assert!(require_window_label("main-shadow", WindowRole::Main).is_err());
        assert!(require_window_label("progress-shadow", WindowRole::Progress).is_err());
    }
}
