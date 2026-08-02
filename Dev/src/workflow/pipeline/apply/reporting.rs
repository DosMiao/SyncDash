//! Reporting for the preservation routes actually used by an Apply run.

use crate::model::plan::Side;

use super::execute::schedule::Shared;

fn in_root_retention_display(sh: &Shared<'_>, side: &Side) -> String {
    if let Some(root) = sh.local_root_of(side) {
        root.display_path()
            .join(crate::foundation::path::to_native(&sh.in_root_keep_rel))
            .display()
            .to_string()
    } else {
        let exec = match side {
            Side::Source => sh.source,
            Side::Target => sh.target,
        };
        format!(
            "{}/{}",
            exec.display().trim_end_matches('/'),
            sh.in_root_keep_rel
        )
    }
}

pub(super) fn report_preservation_routes(sh: &Shared<'_>) {
    use crate::model::event::LogLevel;

    if sh.central_preservation_used() {
        sh.ctx.log(
            LogLevel::Info,
            "apply",
            format!(
                "trash (central; deleted/overwritten originals kept at): {}",
                sh.central_trash_root
                    .as_ref()
                    .expect("central preservation records its root")
                    .display_path()
                    .display()
            ),
        );
    }
    for (side, label) in [(Side::Source, "source"), (Side::Target, "target")] {
        if sh.in_root_preservation_used(&side) {
            sh.ctx.log(
                LogLevel::Info,
                "apply",
                format!(
                    "trash ({label} in-root; deleted/overwritten originals kept at): {}",
                    in_root_retention_display(sh, &side)
                ),
            );
        }
    }
}
