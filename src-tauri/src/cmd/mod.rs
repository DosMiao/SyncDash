//! The Tauri commands, grouped by what they act on.
//!
//! Each one is thin: validate, call the library, project into a DTO. Anything longer than that
//! belongs in `syncdash`, where the CLI can reach it too.

pub mod edit;
pub mod jobs;
pub mod logs;
pub mod results;
pub mod run;
pub mod shell;
