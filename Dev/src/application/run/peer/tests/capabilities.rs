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

    /// The gate must live in the lane. Asserting it through `apply_capabilities` alone proves only
    /// that the report is computable — it cannot catch an entry point that never asks for one, and
    /// that is exactly the shape the CLI peer path had: `run_job` reached the lane through
    /// `run_peer_job`, which ran no check and dropped `--accept-caps`.
    #[test]
    fn a_blocked_peer_apply_is_refused_through_the_lane_before_any_write() {
        use crate::pipeline::guard::caps::CapabilityConsent;
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
            false,
            // Even a blanket --accept-caps cannot buy past a Block; only NeedsAck is consentable.
            &CapabilityConsent::explicit_cli(true),
            &crate::obs::progress::RunCtx::null(),
        );

        assert!(
            !execution.writes_started(),
            "a capability refusal must classify as before-write so the reviewed result survives"
        );
        let error = execution
            .into_result()
            .expect_err("a required marker the protocol cannot prove must refuse the apply");
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    }
}
