//! The capability report: every gap between what the job asks for and what the two backends can
//! give, listed **before** any scanning starts.
//!
//! Blockers refuse the run, `NeedsAck` lines demand explicit consent, `Info` lines go to the log.
//! Nothing degrades silently — that is the whole contract.
//!
//! Primitives only: this layer must not know the Job schema. `Job::read_caps_query` and
//! `Job::write_caps_query` build the queries and hand them down.

use serde::{Deserialize, Serialize};

use crate::model::plan::{Action, Op, Side};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapSeverity {
    /// Running would make the table or the plan lie — refuse outright.
    Block,
    /// Runnable, but only with the user's explicit consent (`--accept-caps` / a ticked box).
    NeedsAck,
    /// Stated for the record; nothing to decide.
    Info,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapItem {
    /// The job feature concerned ("evidence=sampled", "mtime window", …)
    pub feature: String,
    /// "source" | "target" | "both"
    pub side: String,
    pub severity: CapSeverity,
    /// What the job asked for
    pub requested: String,
    /// What the backend can give
    pub actual: String,
    /// What this run will therefore do — in plain words, shown verbatim
    pub effect: String,
}

impl CapItem {
    pub fn render(&self) -> String {
        format!(
            "[{}] {}: wanted {}, backend has {} — {}",
            self.side, self.feature, self.requested, self.actual, self.effect
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapReport {
    pub items: Vec<CapItem>,
}

impl CapReport {
    pub fn blockers(&self) -> Vec<&CapItem> {
        self.items
            .iter()
            .filter(|i| i.severity == CapSeverity::Block)
            .collect()
    }
    pub fn needs_ack(&self) -> Vec<&CapItem> {
        self.items
            .iter()
            .filter(|i| i.severity == CapSeverity::NeedsAck)
            .collect()
    }
    pub fn infos(&self) -> Vec<&CapItem> {
        self.items
            .iter()
            .filter(|i| i.severity == CapSeverity::Info)
            .collect()
    }

    /// A stable, domain-separated identity for exactly the consent-requiring capability facts.
    /// Item insertion order is deliberately irrelevant: adding a backend probe must not invalidate
    /// an otherwise identical grant merely because two independent checks ran in another order.
    pub fn consent_digest(&self, scope: CapabilityScope) -> String {
        let mut items = self.needs_ack();
        items.sort_by(|left, right| {
            (
                left.side.as_str(),
                left.feature.as_str(),
                left.requested.as_str(),
                left.actual.as_str(),
                left.effect.as_str(),
            )
                .cmp(&(
                    right.side.as_str(),
                    right.feature.as_str(),
                    right.requested.as_str(),
                    right.actual.as_str(),
                    right.effect.as_str(),
                ))
        });
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"syncdash-capability-consent-v1\0");
        hash_field(&mut hasher, scope.as_str().as_bytes());
        for item in items {
            hash_field(&mut hasher, item.side.as_bytes());
            hash_field(&mut hasher, item.feature.as_bytes());
            hash_field(&mut hasher, item.requested.as_bytes());
            hash_field(&mut hasher, item.actual.as_bytes());
            hash_field(&mut hasher, item.effect.as_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }

    pub fn consent_satisfied(&self, scope: CapabilityScope, consent: &CapabilityConsent) -> bool {
        self.needs_ack().is_empty()
            || match consent {
                CapabilityConsent::None => false,
                CapabilityConsent::ExactDigest(digest) => digest == &self.consent_digest(scope),
                CapabilityConsent::ExplicitCli => true,
            }
    }
}

fn hash_field(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityScope {
    CompareRead,
    ApplyWrite,
}

impl CapabilityScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CompareRead => "compare_read",
            Self::ApplyWrite => "apply_write",
        }
    }
}

/// Capability consent is explicit about its authority boundary.
///
/// The CLI flag remains intentionally broad for that one foreground invocation. Desktop review
/// uses only `ExactDigest`; a boolean from a webview can never accept a report that changed after
/// it was displayed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum CapabilityConsent {
    #[default]
    None,
    ExactDigest(String),
    ExplicitCli,
}

impl CapabilityConsent {
    pub fn explicit_cli(accepted: bool) -> Self {
        if accepted {
            Self::ExplicitCli
        } else {
            Self::None
        }
    }
}

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
                severity: CapSeverity::Block,
                requested: "symlinks recorded in the table".into(),
                actual: "backend cannot represent symlinks".into(),
                effect:
                    "the table would silently omit every link — refusing to produce a false picture"
                        .into(),
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
                    severity: CapSeverity::NeedsAck,
                    requested: "3-window sampled digests".into(),
                    actual: "no ranged reads".into(),
                    effect: "sampled digests only match other sampled digests, so BOTH sides upgrade to full reads — every changed file is read whole over its link; set evidence=none to skip content reads instead".into(),
                });
            }
        }
    }

    // A widened no-hash window is a blind spot exactly when there is no content evidence
    if q.window_ms > crate::model::plan::MTIME_SLACK_MS {
        let secs = q.window_ms / 1000;
        r.items.push(CapItem {
            feature: "mtime window".into(),
            side: "both".into(),
            severity: if q.hash { CapSeverity::Info } else { CapSeverity::NeedsAck },
            requested: format!("±{}s equality window", crate::model::plan::MTIME_SLACK_MS / 1000),
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

    for (side, side_tag, caps, local) in [
        (Side::Source, "source", src, q.src_local),
        (Side::Target, "target", tgt, q.tgt_local),
    ] {
        // Apply acquires both root leases before it mutates either side, so this gate is not
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
                severity: CapSeverity::Block,
                requested: "safe concurrent-write exclusion".into(),
                actual: actual.into(),
                effect: "the exclusive lease claim cannot be guaranteed — refusing to write; comparing (read-only) still works".into(),
            });
        }

        let side_ops: Vec<&Op> = ops
            .iter()
            .filter(|o| o.side == side && !matches!(o.action, Action::Conflict | Action::Note))
            .collect();
        if side_ops.is_empty() {
            continue;
        }

        // A plan the backend cannot execute in full is a plan the table would lie about
        if caps.unix_mode == Support::No && side_ops.iter().any(|o| o.action == Action::Chmod) {
            r.items.push(CapItem {
                feature: "chmod ops".into(),
                side: side_tag.into(),
                severity: CapSeverity::Block,
                requested: format!("{} permission change(s) from the plan", side_ops.iter().filter(|o| o.action == Action::Chmod).count()),
                actual: "backend has no unix modes".into(),
                effect: "the plan cannot be executed in full — refusing rather than silently dropping ops".into(),
            });
        }
        if caps.symlink == Support::No && side_ops.iter().any(|o| o.link.is_some()) {
            r.items.push(CapItem {
                feature: "symlink ops".into(),
                side: side_tag.into(),
                severity: CapSeverity::Block,
                requested: "symlink creation from the plan".into(),
                actual: "backend cannot create symlinks".into(),
                effect: "the plan cannot be executed in full — refusing rather than silently dropping ops".into(),
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
                severity: CapSeverity::Block,
                requested: "atomic symlink creation without replacement".into(),
                actual: actual.into(),
                effect: "a concurrent entry could be overwritten or misreported — refusing the symlink operation".into(),
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
                severity: CapSeverity::Block,
                requested: "exclusive claim or in-root preservation of an existing entry".into(),
                actual: actual.into(),
                effect: "the operation cannot safely claim or retain the original name — refusing before mutation".into(),
            });
        }

        if q.fsync {
            match caps.fsync {
                Support::No => r.items.push(CapItem {
                    feature: "fsync=true".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::NeedsAck,
                    requested: "fsync before rename".into(),
                    actual: "backend has no fsync".into(),
                    effect: "renamed files may not be durable across a crash on this side — continuing means accepting the server's own caching".into(),
                }),
                Support::Unknown => r.items.push(CapItem {
                    feature: "fsync=true".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::NeedsAck,
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
                    severity: CapSeverity::NeedsAck,
                    requested: "crash-durable rename, publication, and removal entries".into(),
                    actual: "backend cannot durably flush namespace changes".into(),
                    effect: "file contents may survive a crash while their final names do not — continuing accepts that durability gap".into(),
                }),
                Support::Unknown => r.items.push(CapItem {
                    feature: "fsync namespace".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::NeedsAck,
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
                severity: CapSeverity::NeedsAck,
                requested: "re-read the staged file before rename".into(),
                actual: "backend cannot read the staged file back".into(),
                effect: "verification degrades to the copy-stream hash plus a length reconciliation — no on-disk read-back".into(),
            });
        }

        let destructive = side_ops
            .iter()
            .any(|o| matches!(o.action, Action::Delete | Action::Update | Action::Copy));
        if destructive {
            // The version store writes through a retained local-root capability. Protocol roots
            // preserve whole entries in their own trash namespace instead.
            if q.versioning && !local {
                r.items.push(CapItem {
                    feature: "versioning".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::NeedsAck,
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
                    severity: CapSeverity::NeedsAck,
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
            severity: CapSeverity::NeedsAck,
            requested: "chunk-wise delta updates".into(),
            actual: "a root uses a network/VFS backend".into(),
            effect: "delta is disabled this run — updates rewrite files in full over the link"
                .into(),
        });
    }

    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy_to_target() -> Op {
        Op {
            side: Side::Target,
            action: Action::Copy,
            path: "file.txt".into(),
            from: None,
            size: Some(1),
            mtime_ms: Some(1),
            hash: None,
            link: None,
            mode: None,
            reason: "test".into(),
        }
    }

    fn write_report_with_target_caps(target: crate::fs::vfs::VfsCaps) -> CapReport {
        use crate::fs::vfs::{memory::MemVfs, Vfs};

        let source = MemVfs::new("caps-source").caps();
        cap_report_write(
            &WriteCapsQuery {
                fsync: false,
                verify: false,
                versioning: false,
                delta: false,
                src_local: false,
                tgt_local: false,
            },
            &[copy_to_target()],
            &source,
            &target,
        )
    }

    fn item(feature: &str, side: &str, actual: &str) -> CapItem {
        CapItem {
            feature: feature.into(),
            side: side.into(),
            severity: CapSeverity::NeedsAck,
            requested: "requested".into(),
            actual: actual.into(),
            effect: "effect".into(),
        }
    }

    #[test]
    fn consent_digest_is_order_independent_but_field_and_scope_exact() {
        let left = CapReport {
            items: vec![
                item("fsync", "target", "no"),
                item("trash", "source", "network"),
            ],
        };
        let right = CapReport {
            items: vec![
                item("trash", "source", "network"),
                item("fsync", "target", "no"),
            ],
        };
        let digest = left.consent_digest(CapabilityScope::ApplyWrite);
        assert_eq!(digest, right.consent_digest(CapabilityScope::ApplyWrite));
        assert_ne!(digest, left.consent_digest(CapabilityScope::CompareRead));

        let mut changed = right;
        changed.items[0].effect.push_str(" changed");
        assert_ne!(digest, changed.consent_digest(CapabilityScope::ApplyWrite));
    }

    #[test]
    fn exact_consent_accepts_only_the_report_and_scope_that_was_reviewed() {
        let report = CapReport {
            items: vec![item("fsync", "target", "no")],
        };
        let consent =
            CapabilityConsent::ExactDigest(report.consent_digest(CapabilityScope::ApplyWrite));
        assert!(report.consent_satisfied(CapabilityScope::ApplyWrite, &consent));
        assert!(!report.consent_satisfied(CapabilityScope::CompareRead, &consent));
        assert!(!report.consent_satisfied(CapabilityScope::ApplyWrite, &CapabilityConsent::None));
        assert!(
            report.consent_satisfied(CapabilityScope::ApplyWrite, &CapabilityConsent::ExplicitCli)
        );
    }

    #[test]
    fn no_consent_is_needed_when_the_report_has_only_information() {
        let mut informational = item("space", "target", "unobservable");
        informational.severity = CapSeverity::Info;
        let report = CapReport {
            items: vec![informational],
        };
        assert!(report.consent_satisfied(CapabilityScope::ApplyWrite, &CapabilityConsent::None));
    }

    #[test]
    fn root_lock_requires_exclusive_staged_publish_not_set_mtime() {
        use crate::fs::vfs::{memory::MemVfs, Support, Vfs};

        let mut target = MemVfs::new("caps-target").caps();
        target.set_mtime = Support::No;
        target.exclusive_staged_file_publish = Support::Yes;
        let report = write_report_with_target_caps(target);
        assert!(report.items.iter().all(|item| item.feature != "root lock"));
    }

    #[test]
    fn root_lock_fails_closed_when_exclusive_staged_publish_is_not_established() {
        use crate::fs::vfs::{memory::MemVfs, Support, Vfs};

        for (support, expected_actual) in [
            (
                Support::No,
                "backend cannot atomically publish a staged file onto an absent name",
            ),
            (
                Support::Unknown,
                "exclusive staged-file publication is not established for this backend",
            ),
        ] {
            let mut target = MemVfs::new("caps-target").caps();
            target.set_mtime = Support::Yes;
            target.exclusive_staged_file_publish = support;
            let report = write_report_with_target_caps(target);
            let blocker = report
                .blockers()
                .into_iter()
                .find(|item| item.feature == "root lock")
                .expect("missing exclusive staged-file publication must block apply");
            assert_eq!(blocker.actual, expected_actual);
        }
    }

    #[test]
    fn root_lock_checks_both_roots_even_when_only_target_has_operations() {
        use crate::fs::vfs::{memory::MemVfs, Support, Vfs};

        let mut source = MemVfs::new("caps-source").caps();
        source.exclusive_staged_file_publish = Support::No;
        let target = MemVfs::new("caps-target").caps();
        let report = cap_report_write(
            &WriteCapsQuery {
                fsync: false,
                verify: false,
                versioning: false,
                delta: false,
                src_local: false,
                tgt_local: false,
            },
            &[copy_to_target()],
            &source,
            &target,
        );
        assert!(report
            .blockers()
            .into_iter()
            .any(|item| item.feature == "root lock" && item.side == "source"));
    }

    #[test]
    fn existing_entry_rename_is_gated_independently_from_staged_publication() {
        use crate::fs::vfs::{memory::MemVfs, Support, Vfs};

        let mut target = MemVfs::new("caps-target").caps();
        target.exclusive_staged_file_publish = Support::Yes;
        target.exclusive_entry_rename = Support::Unknown;
        let report = write_report_with_target_caps(target);
        assert!(report
            .blockers()
            .into_iter()
            .any(|item| item.feature == "entry rename" && item.side == "target"));
    }

    #[test]
    fn symlink_publication_has_its_own_exclusive_primitive() {
        use crate::fs::vfs::{memory::MemVfs, Support, Vfs};

        let mut target = MemVfs::new("caps-target").caps();
        target.symlink = Support::Yes;
        target.exclusive_symlink_publish = Support::No;
        let mut operation = copy_to_target();
        operation.link = Some("destination".into());
        let source = MemVfs::new("caps-source").caps();
        let report = cap_report_write(
            &WriteCapsQuery {
                fsync: false,
                verify: false,
                versioning: false,
                delta: false,
                src_local: false,
                tgt_local: false,
            },
            &[operation],
            &source,
            &target,
        );
        assert!(report
            .blockers()
            .into_iter()
            .any(|item| item.feature == "symlink publication"));
    }

    #[test]
    fn file_flush_does_not_stand_in_for_namespace_durability() {
        use crate::fs::vfs::{memory::MemVfs, Support, Vfs};

        let source = MemVfs::new("caps-source").caps();
        let mut target = MemVfs::new("caps-target").caps();
        target.fsync = Support::Yes;
        target.durable_namespace = Support::Unknown;
        let report = cap_report_write(
            &WriteCapsQuery {
                fsync: true,
                verify: false,
                versioning: false,
                delta: false,
                src_local: false,
                tgt_local: false,
            },
            &[copy_to_target()],
            &source,
            &target,
        );
        assert!(report.items.iter().any(|item| {
            item.feature == "fsync namespace"
                && item.side == "target"
                && item.severity == CapSeverity::NeedsAck
        }));
    }
}
