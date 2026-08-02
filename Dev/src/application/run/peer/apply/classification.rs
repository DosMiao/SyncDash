pub(in crate::run::peer) fn classify_peer_completion(
    writes_started: bool,
    result: std::io::Result<crate::obs::progress::ApplyOutcome>,
) -> crate::run::ApplyExecution {
    match (writes_started, result) {
        (false, Ok(outcome)) => crate::run::ApplyExecution::rejected(outcome),
        (true, Ok(outcome)) => crate::run::ApplyExecution::started(outcome),
        (false, Err(error)) => crate::run::ApplyExecution::failed_before_write(error),
        (true, Err(error)) => crate::run::ApplyExecution::failed_after_write(error),
    }
}
