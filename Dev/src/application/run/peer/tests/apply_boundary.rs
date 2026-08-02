mod apply_boundary_tests {
    use crate::obs::progress::ApplyOutcome;
    use crate::run::peer::apply::classify_peer_completion;

    #[test]
    fn peer_boundary_is_explicit_not_inferred_from_outcome_or_error_text() {
        let indistinguishable = ApplyOutcome {
            done: 0,
            skipped: 1,
            errors: 1,
            bytes_copied: 0,
            cancelled: false,
        };
        let before = classify_peer_completion(false, Ok(indistinguishable));
        let after = classify_peer_completion(true, Ok(indistinguishable));
        assert!(!before.writes_started());
        assert!(after.writes_started());

        let before = classify_peer_completion(
            false,
            Err(std::io::Error::other("identical transport failure")),
        );
        let after = classify_peer_completion(
            true,
            Err(std::io::Error::other("identical transport failure")),
        );
        assert!(!before.writes_started());
        assert!(after.writes_started());
        assert_eq!(
            before.into_result().unwrap_err().to_string(),
            after.into_result().unwrap_err().to_string()
        );
    }
}
