//! Capability reporting and consent behavior.

use super::consent::*;
use super::report::*;
use super::write::*;
use crate::model::plan::{Action, Op, Side};

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
