//! The Tauri commands, grouped by what they act on.
//!
//! Each one is thin: validate, call the library, project into a DTO. Anything longer than that
//! belongs in `syncdash`, where the CLI can reach it too.

pub mod autoscan;
pub mod edit;
pub mod jobs;
pub mod logs;
pub mod results;
pub mod run;
pub mod shell;

pub(crate) fn require_main_window(window: &tauri::WebviewWindow) -> Result<(), String> {
    require_main_label(window.label())
}

fn require_main_label(label: &str) -> Result<(), String> {
    if label == "main" {
        Ok(())
    } else {
        Err("This operation can only be authorized from the main window".into())
    }
}

#[cfg(test)]
mod tests {
    use super::require_main_label;

    #[test]
    fn only_the_main_webview_can_authorize_operations() {
        assert!(require_main_label("main").is_ok());
        assert!(require_main_label("progress").is_err());
        assert!(require_main_label("main-shadow").is_err());
    }
}
