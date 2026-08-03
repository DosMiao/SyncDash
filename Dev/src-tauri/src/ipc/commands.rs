//! The Tauri commands, grouped by what they act on.
//!
//! Each one is thin: authorize the calling window, extract state and arguments, delegate, and
//! project the result into a DTO. Desktop policy, authorization, and evidence rules belong in
//! `features/`; only behavior the CLI must reach too belongs in `syncdash`.

pub(crate) mod autoscan;
pub(crate) mod compare;
pub(crate) mod desktop;
pub(crate) mod job_editor;
pub(crate) mod jobs;
pub(crate) mod logs;
pub(crate) mod operations;
pub(crate) mod settings;
