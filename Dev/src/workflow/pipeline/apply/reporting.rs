//! Reporting for an Apply run: how one op is named in the log, and which preservation routes the
//! run actually used.

use crate::model::plan::{Op, Side};

use super::execute::schedule::Shared;

/// The verbose log line for a single op. The dry-run preview and the executed-outcome record must
/// name the same op identically, otherwise a `DRY` line cannot be matched against the `OK`/`ERR`
/// line it predicted.
pub(super) fn op_label(op: &Op) -> String {
    format!("[{}] {:?} {}", op.side.as_str(), op.action, op.path)
}

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
    for side in [Side::Source, Side::Target] {
        if sh.in_root_preservation_used(&side) {
            let label = side.as_str();
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
