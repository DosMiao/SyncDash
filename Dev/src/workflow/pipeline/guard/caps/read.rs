//! Capability review for the scanning side.

use super::report::{CapItem, CapReport, CapSeverity};

/// What the read side of a run asks of its backends — primitives only (this layer
/// must not know the Job schema; `Job::read_caps_query` builds one).
#[derive(Clone, Copy, Debug)]
pub struct ReadCapsQuery {
    pub hash: bool,
    pub sampled: bool,
    pub escalate: bool,
    pub symlinks_direct: bool,
    pub min_free_pct: f64,
    /// The effective no-hash mtime window (already widened to the coarser backend)
    pub window_ms: i64,
    pub src_local: bool,
    pub tgt_local: bool,
}

/// The read-side capability comparison. Write-side items (fsync, verify, trash,
/// delta, chmod) join when the VFS apply lane lands.
pub fn cap_report_read(
    q: &ReadCapsQuery,
    src: &crate::fs::vfs::VfsCaps,
    tgt: &crate::fs::vfs::VfsCaps,
) -> CapReport {
    use crate::fs::vfs::Support;
    let mut r = CapReport::default();

    // symlinks="direct" needs the backend to *see* links, or the table paints a false picture
    for (side, c) in [("source", src), ("target", tgt)] {
        if q.symlinks_direct && c.symlink == Support::No {
            r.items.push(CapItem {
                feature: "symlinks=direct".into(),
                side: side.into(),
                severity: CapSeverity::Unavailable,
                requested: "symlinks recorded in the table".into(),
                actual: "backend cannot represent symlinks".into(),
                effect: "the table omits every link on this side, so links are invisible to this comparison".into(),
            });
        }
    }

    // sampled evidence needs ranged reads; without them the scan upgrades to full reads, out loud
    if q.hash && q.sampled {
        for (side, c) in [("source", src), ("target", tgt)] {
            if c.ranged_read == Support::No {
                r.items.push(CapItem {
                    feature: "evidence=sampled".into(),
                    side: side.into(),
                    severity: CapSeverity::Degraded,
                    requested: "3-window sampled digests".into(),
                    actual: "no ranged reads".into(),
                    effect: "sampled digests only match other sampled digests, so BOTH sides upgrade to full reads — every changed file is read whole over its link; set evidence=none to skip content reads instead".into(),
                });
            }
        }
    }

    // A widened no-hash window is a blind spot exactly when there is no content evidence
    if q.window_ms > crate::pipeline::compare::MTIME_SLACK_MS {
        let secs = q.window_ms / 1000;
        r.items.push(CapItem {
            feature: "mtime window".into(),
            side: "both".into(),
            severity: if q.hash { CapSeverity::Info } else { CapSeverity::Degraded },
            requested: format!("±{}s equality window", crate::pipeline::compare::MTIME_SLACK_MS / 1000),
            actual: format!("backend timestamps are only ±{secs}s precise"),
            effect: if q.hash {
                format!("timestamp equality widens to ±{secs}s; content evidence still decides")
            } else {
                format!("with evidence=none, edits within {secs}s of each other are INVISIBLE to this comparison")
            },
        });
    }

    if q.escalate && q.sampled && (!q.src_local || !q.tgt_local) {
        r.items.push(CapItem {
            feature: "escalate".into(),
            side: "both".into(),
            severity: CapSeverity::Info,
            requested: "full re-read on sampled-digest/mtime disagreement".into(),
            actual: "a root uses a network/VFS backend".into(),
            effect: "escalation is skipped this run (it arrives with the VFS write lane)".into(),
        });
    }

    if q.min_free_pct > 0.0 {
        for (side, c) in [("source", src), ("target", tgt)] {
            if c.free_space == Support::No {
                r.items.push(CapItem {
                    feature: "min_free_pct".into(),
                    side: side.into(),
                    severity: CapSeverity::Info,
                    requested: "free-space gate before writing".into(),
                    actual: "backend cannot report free space".into(),
                    effect: "the space gate is inert on this side — writes proceed without it"
                        .into(),
                });
            }
        }
    }

    r
}
