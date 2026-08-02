use crate::job::Job;

/// Build the peer-side scan command from the job's fully resolved evidence and filter policy.
pub(in crate::run::peer) fn build_peer_scan_arguments(job: &Job, peer_root: &str) -> Vec<String> {
    // The job's own tier, not a narrowed one: this process holds no handle on the far root, so there
    // is no second backend to negotiate down to.
    let opt = crate::run::scan_opts(job);
    let mut arguments: Vec<String> = vec![
        "scan".into(),
        peer_root.to_string(),
        "--evidence".into(),
        crate::run::evidence_label(&opt).into(),
        "--cache".into(),
        (if opt.use_cache { "on" } else { "off" }).into(),
        "--junk".into(),
        "none".into(),
    ];
    for include in &job.include {
        arguments.push("--include".into());
        arguments.push(include.clone());
    }
    for exclude in &job.exclude {
        arguments.push("--exclude".into());
        arguments.push(exclude.clone());
    }
    if job.symlinks == "direct" {
        arguments.push("--symlinks-direct".into());
    }
    arguments
}
