mod filter_tests {
    use crate::job::Job;
    use crate::run::peer::configuration::build_peer_scan_arguments;

    fn job_with(include: &[&str], exclude: &[&str]) -> Job {
        Job {
            mode: "mirror".into(),
            source: r"D:\src".into(),
            targets: vec!["peer://mac/Users/ben/dst".into()],
            include: include.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            ..Job::default()
        }
    }

    fn pairs<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
        args.windows(2)
            .filter(|w| w[0] == flag)
            .map(|w| w[1].as_str())
            .collect()
    }

    #[test]
    fn every_exclude_crosses_the_link() {
        let arguments = build_peer_scan_arguments(
            &job_with(&[], &["*/big_temp/", "*/*.log"]),
            "/Users/ben/dst",
        );
        assert_eq!(
            pairs(&arguments, "--exclude"),
            vec!["*/big_temp/", "*/*.log"]
        );
        assert_eq!(pairs(&arguments, "--junk"), vec!["none"]);
    }

    /// The whole filter has to cross, not half of it.
    ///
    /// `include` is an allowlist: with one set, everything outside it is *not part of this job*.
    /// The local side applies it. If the far side never hears about it, the far side reports files
    /// the local filter hid — and because they are then "on the target and not on the source",
    /// `mirror` proposes a `Delete` for every one of them. That is the exact failure the sibling
    /// `--junk none` line three lines up exists to prevent, and it is data loss, not a cosmetic
    /// asymmetry.
    #[test]
    fn every_include_crosses_the_link_too() {
        let arguments =
            build_peer_scan_arguments(&job_with(&["*/keep/", "/docs/"], &[]), "/Users/ben/dst");
        assert_eq!(
            pairs(&arguments, "--include"),
            vec!["*/keep/", "/docs/"],
            "an allowlist that binds only the local root turns every unlisted peer file into a deletion"
        );
    }
}
