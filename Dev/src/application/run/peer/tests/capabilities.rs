mod apply_capability_tests {
    use crate::job::Job;
    use crate::model::plan::{Action, Op, Side};
    use crate::pipeline::guard::caps::CapSeverity;
    use crate::run::peer::apply_capabilities;

    /// The gate under test reads only the job and the ops; the header is required to build a
    /// `Plan` and is otherwise inert here.
    fn peer_plan_header(ops: usize) -> crate::model::plan::PlanHeader {
        crate::model::plan::PlanHeader {
            schema: crate::model::plan::PLAN_SCHEMA,
            kind: "plan".into(),
            mode: "mirror".into(),
            generated_at_ms: 1_700_000_000_000,
            source_root: "/source".into(),
            source_host: "local".into(),
            target_root: "peer://host/srv/data".into(),
            target_host: "host".into(),
            op_count: ops as u64,
            conflict_count: 0,
            source_entries: 1,
            target_entries: 1,
            source_excluded: 0,
            target_excluded: 0,
            source_walk_errors: 0,
            target_walk_errors: 0,
            source_walk_err_samples: Vec::new(),
            target_walk_err_samples: Vec::new(),
            source_icloud_stubs: 0,
            target_icloud_stubs: 0,
            source_icloud_stub_samples: Vec::new(),
            target_icloud_stub_samples: Vec::new(),
        }
    }

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
            item.feature == "min_free_pct" && item.severity == CapSeverity::Degraded
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
            item.feature == "require_marker" && item.severity == CapSeverity::Unavailable
        }));
    }

    /// An unprovable safeguard is reported, not enforced: the lane runs on and whatever happens
    /// next is the transport's own business. Pinned because the previous shape refused here, and a
    /// silent return of that refusal would strand a peer job with no way past it.
    #[test]
    fn an_unprovable_peer_safeguard_no_longer_withholds_the_lane() {
        use crate::run::peer::apply_peer_job_with_classified;

        let job = Job {
            source: "/source".into(),
            targets: vec!["peer://host/srv/data".into()],
            require_marker: true,
            ..Job::default()
        };
        let selected = job.select_target(0).unwrap();
        let ops = [target_write()];
        let plan = crate::model::plan::Plan {
            header: peer_plan_header(ops.len()),
            ops: ops.to_vec(),
        };

        let execution = apply_peer_job_with_classified(
            "test",
            &selected,
            &plan,
            &ops,
            false,
            &crate::obs::progress::RunCtx::null(),
        );

        let error = execution
            .into_result()
            .expect_err("this peer host does not exist, so the run fails reaching it");
        assert_ne!(
            error.kind(),
            std::io::ErrorKind::Unsupported,
            "the unprovable marker must not be what stops the run: {error}"
        );
    }
}
