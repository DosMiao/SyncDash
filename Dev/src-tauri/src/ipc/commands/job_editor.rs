//! What the job editor needs while a job is being edited: whether the roots look right, what a
//! filter mask would hit, and the junk presets available to tick.

use crate::contracts::jobs::JunkPresetDto;
use crate::contracts::paths::PathVerdict;
use crate::ipc::{require_window_role, WindowRole};

/// Live health check for the editor: whether the path exists, whether it is a directory, whether it
/// carries a mount-point marker, and how the two roots relate (identical / nested). A mistyped path costs
/// too much to be reported only in the status bar once Compare runs.
#[tauri::command]
pub fn inspect_paths(
    window: tauri::WebviewWindow,
    source: String,
    target: String,
) -> Result<PathVerdict, String> {
    require_window_role(&window, WindowRole::Main)?;
    Ok(crate::features::jobs::editor::inspect_paths(source, target))
}

/// Ad-hoc mask matching for Advanced Filters. The frontend **does not write its own glob** — the FFS mask
/// semantics have exactly one implementation, in filter.rs, so a mask tried out in the UI behaves identically once written into the job's exclude.
#[tauri::command]
pub fn mask_match(
    window: tauri::WebviewWindow,
    masks: Vec<String>,
    paths: Vec<String>,
) -> Result<Vec<bool>, String> {
    require_window_role(&window, WindowRole::Main)?;
    Ok(syncdash::pipeline::filter::mask_hits(&masks, &paths))
}

/// The junk presets, patterns and all. The frontend **does not carry its own copy of these lists** —
/// the editor's checkboxes write literally what the engine would have applied, which is the whole
/// reason a checkbox can now be trusted to describe what a job excludes.
#[tauri::command]
pub fn junk_presets(window: tauri::WebviewWindow) -> Result<Vec<JunkPresetDto>, String> {
    require_window_role(&window, WindowRole::Main)?;
    Ok(syncdash::job::junk::JUNK_PRESETS
        .iter()
        .map(|p| JunkPresetDto {
            id: p.id.to_string(),
            label: p.label.to_string(),
            hint: p.hint.to_string(),
            patterns: p.patterns.iter().map(|s| s.to_string()).collect(),
        })
        .collect())
}
