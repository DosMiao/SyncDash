mod apply_capability_tests {
    use crate::job::Job;
    use crate::model::plan::{Action, Op, Side};
    use crate::pipeline::guard::caps::CapSeverity;
    use crate::run::peer::apply_capabilities;

    fn target_write() -> Op {
        Op {
            side: Side::Target,
            action: Action::Delete,
            path: "old.txt".into(),
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "test".into(),
        }
    }

    #[test]
    fn peer_limitations_are_structured_only_when_the_peer_side_writes() {
        let job = Job {
            source: "/source".into(),
            targets: vec!["peer://host/srv/data".into()],
            ..Job::default()
        };
        let selected = job.select_target(0).unwrap();
        let report = apply_capabilities(&selected, &[target_write()]);
        assert!(report.items.iter().any(|item| {
            item.feature == "min_free_pct" && item.severity == CapSeverity::NeedsAck
        }));

        let mut source = target_write();
        source.side = Side::Source;
        assert!(apply_capabilities(&selected, &[source]).items.is_empty());
    }

    #[test]
    fn an_unobservable_required_peer_marker_is_a_hard_blocker() {
        let job = Job {
            source: "/source".into(),
            targets: vec!["peer://host/srv/data".into()],
            require_marker: true,
            ..Job::default()
        };
        let selected = job.select_target(0).unwrap();
        let report = apply_capabilities(&selected, &[target_write()]);
        assert!(report.items.iter().any(|item| {
            item.feature == "require_marker" && item.severity == CapSeverity::Block
        }));
    }
}
