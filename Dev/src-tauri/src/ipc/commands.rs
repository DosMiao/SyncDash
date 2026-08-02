//! The Tauri commands, grouped by what they act on.
//!
//! Each one is thin: validate, call the library, project into a DTO. Anything longer than that
//! belongs in `syncdash`, where the CLI can reach it too.

pub(crate) mod autoscan;
pub(crate) mod compare;
pub(crate) mod desktop;
pub(crate) mod job_editor;
pub(crate) mod jobs;
pub(crate) mod logs;
pub(crate) mod operations;
pub(crate) mod settings;
