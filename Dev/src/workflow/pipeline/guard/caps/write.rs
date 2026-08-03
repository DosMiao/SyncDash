//! Capability review for the writing side.
//!
//! The larger half, because writing is where a missing capability changes what happens to data: no
//! exclusive publish, no rename-overwrite, no mtime preservation, or a timestamp coarser than the
//! compare window assumes.

use super::report::{CapItem, CapReport, CapSeverity};
use crate::model::plan::{Action, Op, Side};

/// What the write side of a run asks of its backends (`Job::write_caps_query` builds one).
#[derive(Clone, Copy, Debug)]
pub struct WriteCapsQuery {
    pub fsync: bool,
    pub verify: bool,
    pub versioning: bool,
    pub delta: bool,
    pub src_local: bool,
    pub tgt_local: bool,
}

/// The write-side capability comparison, evaluated against the ops that will actually
/// run. Joins the read-side report before the apply stage starts.
pub fn cap_report_write(
    q: &WriteCapsQuery,
    ops: &[Op],
    src: &crate::fs::vfs::VfsCaps,
    tgt: &crate::fs::vfs::VfsCaps,
) -> CapReport {
    use crate::fs::vfs::Support;
    let mut r = CapReport::default();

    for (side, caps, local) in [
        (Side::Source, src, q.src_local),
        (Side::Target, tgt, q.tgt_local),
    ] {
        let side_tag = side.as_str();
        // Apply acquires both root leases before it mutates either side, so this item is not
        // conditional on this particular side having a visible operation in the plan.
        if caps.exclusive_staged_file_publish != Support::Yes {
            let actual = match caps.exclusive_staged_file_publish {
                Support::No => {
                    "backend cannot atomically publish a staged file onto an absent name"
                }
                Support::Unknown => {
                    "exclusive staged-file publication is not established for this backend"
                }
                Support::Yes => unreachable!(),
            };
            r.items.push(CapItem {
                feature: "root lock".into(),
                side: side_tag.into(),
                severity: CapSeverity::Unavailable,
                requested: "safe concurrent-write exclusion".into(),
                actual: actual.into(),
                effect: "the lease is still claimed when apply starts; a backend that cannot claim it exclusively fails the run there, before anything is written".into(),
            });
        }

        let side_ops: Vec<&Op> = ops
            .iter()
            .filter(|o| o.side == side && o.action.is_executable())
            .collect();
        if side_ops.is_empty() {
            continue;
        }

        // A plan the backend cannot execute in full is a plan the table would otherwise lie about,
        // so each such shortfall is named with the exact number of rows it covers.
        if caps.unix_mode == Support::No && side_ops.iter().any(|o| o.action == Action::Chmod) {
            r.items.push(CapItem {
                feature: "chmod ops".into(),
                side: side_tag.into(),
                severity: CapSeverity::Unavailable,
                requested: format!("{} permission change(s) from the plan", side_ops.iter().filter(|o| o.action == Action::Chmod).count()),
                actual: "backend has no unix modes".into(),
                effect: "those permission changes are skipped on this side; every other operation in the plan still runs".into(),
            });
        }
        if caps.symlink == Support::No && side_ops.iter().any(|o| o.link.is_some()) {
            r.items.push(CapItem {
                feature: "symlink ops".into(),
                side: side_tag.into(),
                severity: CapSeverity::Unavailable,
                requested: "symlink creation from the plan".into(),
                actual: "backend cannot create symlinks".into(),
                effect: "each link operation is attempted and reported as a failed path; the rest of the plan still runs".into(),
            });
        }

        if side_ops.iter().any(|operation| operation.link.is_some())
            && caps.exclusive_symlink_publish != Support::Yes
        {
            let actual = match caps.exclusive_symlink_publish {
                Support::No => "backend cannot publish a symlink exclusively onto an absent name",
                Support::Unknown => {
                    "exclusive symlink publication is not established for this backend"
                }
                Support::Yes => unreachable!(),
            };
            r.items.push(CapItem {
                feature: "symlink publication".into(),
                side: side_tag.into(),
                severity: CapSeverity::Unavailable,
                requested: "atomic symlink creation without replacement".into(),
                actual: actual.into(),
                effect: "a symlink that cannot claim its name exclusively could overwrite or misreport a concurrent entry".into(),
            });
        }

        let needs_entry_rename = side_ops.iter().any(|operation| {
            matches!(
                operation.action,
                Action::Move | Action::Copy | Action::Update | Action::Delete
            )
        });
        if needs_entry_rename && caps.exclusive_entry_rename != Support::Yes {
            let actual = match caps.exclusive_entry_rename {
                Support::No => "backend has no atomic existing-entry no-replace rename",
                Support::Unknown => {
                    "atomic existing-entry no-replace rename is not established for this backend"
                }
                Support::Yes => unreachable!(),
            };
            r.items.push(CapItem {
                feature: "entry rename".into(),
                side: side_tag.into(),
                severity: CapSeverity::Unavailable,
                requested: "exclusive claim or in-root preservation of an existing entry".into(),
                actual: actual.into(),
                effect: "an operation that cannot safely claim or retain its original name fails that path instead of publishing it".into(),
            });
        }

        if q.fsync {
            match caps.fsync {
                Support::No => r.items.push(CapItem {
                    feature: "fsync=true".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::Degraded,
                    requested: "fsync before rename".into(),
                    actual: "backend has no fsync".into(),
                    effect: "renamed files may not be durable across a crash on this side — continuing means accepting the server's own caching".into(),
                }),
                Support::Unknown => r.items.push(CapItem {
                    feature: "fsync=true".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::Degraded,
                    requested: "fsync before rename".into(),
                    actual: "support unknown until tried".into(),
                    effect: "fsync is attempted per file; where the server refuses it, that file counts as failed (set fsync=false to skip the attempt)".into(),
                }),
                Support::Yes => {}
            }
            match caps.durable_namespace {
                Support::No => r.items.push(CapItem {
                    feature: "fsync namespace".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::Degraded,
                    requested: "crash-durable rename, publication, and removal entries".into(),
                    actual: "backend cannot durably flush namespace changes".into(),
                    effect: "file contents may survive a crash while their final names do not — continuing accepts that durability gap".into(),
                }),
                Support::Unknown => r.items.push(CapItem {
                    feature: "fsync namespace".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::Degraded,
                    requested: "crash-durable rename, publication, and removal entries".into(),
                    actual: "namespace durability is not established for this backend".into(),
                    effect: "the backend may acknowledge file flushes without durably recording the directory entries — continuing accepts that uncertainty".into(),
                }),
                Support::Yes => {}
            }
        }

        if q.verify && caps.read_back == Support::No {
            r.items.push(CapItem {
                feature: "verify_writes".into(),
                side: side_tag.into(),
                severity: CapSeverity::Degraded,
                requested: "re-read the staged file before rename".into(),
                actual: "backend cannot read the staged file back".into(),
                effect: "verification degrades to the copy-stream hash plus a length reconciliation — no on-disk read-back".into(),
            });
        }

        // Only an operation that displaces existing data can send anything to a trash or version
        // store. `Copy` publishes onto a name that compare found absent and `DeleteDir` removes
        // only an already-empty directory, so neither preserves an original — reporting a
        // preservation effect for a plan made purely of them describes a run that cannot happen.
        let destructive = side_ops
            .iter()
            .any(|o| matches!(o.action, Action::Delete | Action::Update));
        if destructive {
            // The version store writes through a retained local-root capability. Protocol roots
            // preserve whole entries in their own trash namespace instead.
            if q.versioning && !local {
                r.items.push(CapItem {
                    feature: "versioning".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::Degraded,
                    requested: "rdelta version store on this root".into(),
                    actual: "network/VFS backend (no local version machinery)".into(),
                    effect: "overwritten/deleted files are kept as whole files under <root>/.syncdash/trash/<run>/ instead — recover with any file browser; rdelta history does not accrue on this side".into(),
                });
            } else if !q.versioning && !caps.local_trash {
                // Deliberately not gated on `local`: a mounted share can expose a local
                // capability while the central store remains on another volume or machine.
                r.items.push(CapItem {
                    feature: "trash".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::Degraded,
                    requested: "deleted/overwritten files into the central trash store".into(),
                    // Reads through the template as "wanted …, backend has a network share, the
                    // central store is on this machine — …"
                    actual: format!("a {}, and the central store is on this machine", caps.medium.as_str()),
                    effect: "originals are renamed into <root>/.syncdash/trash/<run>/ on the root's own side instead — recoverable with any file browser, and nothing crosses the link".into(),
                });
            }
        }
    }

    if q.delta && !(q.src_local && q.tgt_local) {
        r.items.push(CapItem {
            feature: "delta".into(),
            side: "both".into(),
            severity: CapSeverity::Degraded,
            requested: "chunk-wise delta updates".into(),
            actual: "a root uses a network/VFS backend".into(),
            effect: "delta is disabled this run — updates rewrite files in full over the link"
                .into(),
        });
    }

    r
}
