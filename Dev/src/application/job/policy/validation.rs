//! Validation of persisted settings and relationships among root phrases.

use crate::job::model::{Job, SingleTargetJob};

impl Job {
    /// Validate every persisted engine setting before the job can be compared or written.
    pub fn validate(&self) -> Result<(), String> {
        validate_choice("mode", &self.mode, &["mirror", "sync", "enrich"])?;
        validate_choice(
            "rigor",
            &self.rigor,
            &[
                "quick", "fast", "balanced", "standard", "paranoid", "custom",
            ],
        )?;
        if let Some(evidence) = self.evidence.as_deref() {
            validate_choice("evidence", evidence, &["none", "sampled", "full"])?;
        }
        validate_choice("symlinks", &self.symlinks, &["exclude", "direct"])?;
        validate_choice(
            "on_conflict",
            &self.on_conflict,
            &["report", "copy", "newer"],
        )?;

        validate_ratio("min_free_pct", self.min_free_pct)?;
        validate_non_negative("max_delete_ratio", self.max_delete_ratio)?;
        if self.max_conflicts < -1 {
            return Err("max_conflicts must be -1 (unlimited) or zero or greater".into());
        }
        if let Some(parallel) = self.parallel {
            if !(1..=16).contains(&parallel) {
                return Err("parallel must be between 1 and 16".into());
            }
        }
        if self.autoscan_interval_secs == Some(0) {
            return Err(
                "autoscan_interval_secs must be at least 1 when AutoScan is enabled".into(),
            );
        }
        if self.autoscan_auto_apply && self.autoscan_interval_secs.is_none() {
            return Err("autoscan_auto_apply requires autoscan_interval_secs".into());
        }

        self.validate_target_rules()?;
        validate_root_relationships(&self.source, &self.targets)
    }

    fn validate_target_rules(&self) -> Result<(), String> {
        if self.targets.is_empty() {
            return Err("a job must contain at least one target root".into());
        }
        if self.targets.len() > 1 {
            if self.mode == "sync" {
                return Err("sync mode does not support multiple targets (N-way merge needs version-vector attribution) — use paired sync jobs instead (hub-and-spoke)".into());
            }
            if self
                .targets
                .iter()
                .any(|target| crate::fs::vfs::spec::is_peer(target))
            {
                return Err("peer:// targets do not support multiple targets yet".into());
            }
        }
        self.validate_roots()
    }

    /// Root-phrase sanity: unknown `xyz://` schemes are refused, never silently treated as a local
    /// path, and a peer root may only be the target.
    pub fn validate_roots(&self) -> Result<(), String> {
        use crate::fs::vfs::spec::{is_peer, parse, RootSpec, KNOWN_SCHEMES};
        if self.source.trim().is_empty() {
            return Err("source root cannot be empty".into());
        }
        if self.targets.iter().any(|target| target.trim().is_empty()) {
            return Err("target root cannot be empty".into());
        }
        for (label, root) in std::iter::once(("source", &self.source))
            .chain(self.targets.iter().map(|target| ("target", target)))
        {
            match parse(root) {
                RootSpec::UnknownScheme { scheme, .. } => {
                    return Err(format!(
                        "{label} '{root}': unknown scheme '{scheme}://' — refusing to treat it as a local path (known: {})",
                        KNOWN_SCHEMES.join(", ")
                    ));
                }
                RootSpec::Endpoint(endpoint) if endpoint.host.trim().is_empty() => {
                    return Err(format!("{label} '{root}': endpoint host cannot be empty"));
                }
                RootSpec::Endpoint(endpoint)
                    if endpoint.root.split('/').any(|segment| segment == "..") =>
                {
                    return Err(format!(
                        "{label} '{root}': endpoint root cannot contain a '..' segment"
                    ));
                }
                _ => {}
            }
        }
        // The peer lane is directional: this side builds a package and the far side applies it.
        if is_peer(&self.source) {
            return Err(format!(
                "source '{}': a peer root can only be the target — this side packs, the far side applies",
                self.source
            ));
        }
        Ok(())
    }

    /// Bind the persisted configuration to exactly one target before entering a phrase-based
    /// compare or apply pipeline.
    pub fn select_target(&self, target_index: usize) -> Result<SingleTargetJob, String> {
        self.validate()?;
        let target = self.targets.get(target_index).cloned().ok_or_else(|| {
            format!(
                "target index {target_index} is out of range ({} total)",
                self.targets.len()
            )
        })?;
        let mut configuration = self.clone();
        configuration.targets = vec![target];
        Ok(SingleTargetJob {
            configuration,
            target_index,
        })
    }
}

fn validate_choice(field: &str, value: &str, accepted: &[&str]) -> Result<(), String> {
    if accepted.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "{field} must be one of {} (got '{value}')",
            accepted.join(", ")
        ))
    }
}

fn validate_ratio(field: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() {
        return Err(format!("{field} must be a finite number"));
    }
    if !(0.0..=1.0).contains(&value) {
        return Err(format!("{field} must be between 0 and 1"));
    }
    Ok(())
}

fn validate_non_negative(field: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() {
        return Err(format!("{field} must be a finite number"));
    }
    if value < 0.0 {
        return Err(format!("{field} must be zero or greater"));
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
struct ComparableLocalRoot {
    prefix: String,
    segments: Vec<String>,
}

#[derive(PartialEq, Eq)]
struct ComparableEndpointRoot {
    endpoint: String,
    segments: Vec<String>,
}

fn comparable_endpoint_root(raw: &str) -> Option<ComparableEndpointRoot> {
    use crate::fs::vfs::spec::{default_port, parse, RootSpec};
    let RootSpec::Endpoint(endpoint_spec) = parse(raw) else {
        return None;
    };
    let user = endpoint_spec.user.as_deref().unwrap_or("");
    let endpoint = format!(
        "{}://{}@{}:{}",
        endpoint_spec.scheme,
        user,
        endpoint_spec.host.to_lowercase(),
        endpoint_spec
            .port
            .unwrap_or(default_port(&endpoint_spec.scheme)),
    );
    let case_insensitive = endpoint_spec.scheme == "smb";
    let segments = endpoint_spec
        .root
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .map(|segment| {
            if case_insensitive {
                segment.to_lowercase()
            } else {
                segment.to_string()
            }
        })
        .collect();
    Some(ComparableEndpointRoot { endpoint, segments })
}

fn comparable_local_root(raw: &str) -> Option<ComparableLocalRoot> {
    use crate::fs::vfs::spec::{parse, RootSpec};
    let RootSpec::Local(_) = parse(raw) else {
        return None;
    };

    let unified = raw.trim().replace('\\', "/");
    let windows =
        cfg!(windows) || unified.starts_with("//") || unified.as_bytes().get(1) == Some(&b':');
    let normalized = if windows {
        unified.to_lowercase()
    } else {
        unified
    };
    let prefix = if normalized.starts_with("//") {
        "//"
    } else if normalized.starts_with('/') {
        "/"
    } else if normalized.as_bytes().get(1) == Some(&b':') {
        &normalized[..2]
    } else {
        ""
    };
    let body = normalized
        .strip_prefix(prefix)
        .unwrap_or(&normalized)
        .trim_start_matches('/');
    let mut segments = Vec::new();
    for segment in body
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
    {
        if segment == ".." {
            if matches!(segments.last().map(String::as_str), Some(last) if last != "..") {
                segments.pop();
            } else if prefix.is_empty() {
                segments.push(segment.to_string());
            }
        } else {
            segments.push(segment.to_string());
        }
    }
    Some(ComparableLocalRoot {
        prefix: prefix.to_string(),
        segments,
    })
}

fn validate_root_relationships(source: &str, targets: &[String]) -> Result<(), String> {
    let source_local = comparable_local_root(source);
    let source_endpoint = comparable_endpoint_root(source);
    let source_identity = root_identity(source);
    let mut seen = Vec::<String>::new();
    for (index, target) in targets.iter().enumerate() {
        let identity = root_identity(target);
        if identity == source_identity {
            return Err("source and target must be different directories".into());
        }
        if seen.iter().any(|existing| existing == &identity) {
            return Err(format!("target {} duplicates an earlier target", index + 1));
        }
        seen.push(identity);

        if let (Some(source_root), Some(target_root)) =
            (source_local.as_ref(), comparable_local_root(target))
        {
            if source_root.prefix == target_root.prefix {
                if target_root.segments.starts_with(&source_root.segments) {
                    return Err("target cannot be nested inside source".into());
                }
                if source_root.segments.starts_with(&target_root.segments) {
                    return Err("source cannot be nested inside target".into());
                }
            }
        }

        if let (Some(source_root), Some(target_root)) =
            (source_endpoint.as_ref(), comparable_endpoint_root(target))
        {
            if source_root.endpoint == target_root.endpoint {
                if target_root.segments.starts_with(&source_root.segments) {
                    return Err("target cannot be nested inside source".into());
                }
                if source_root.segments.starts_with(&target_root.segments) {
                    return Err("source cannot be nested inside target".into());
                }
            }
        }
    }
    Ok(())
}

pub fn validate_root_pair(source: &str, target: &str) -> Result<(), String> {
    validate_root_relationships(source, &[target.to_string()])
}

fn root_identity(raw: &str) -> String {
    match crate::fs::vfs::spec::parse(raw) {
        crate::fs::vfs::spec::RootSpec::Endpoint(endpoint) => {
            format!("endpoint:{}", endpoint.identity())
        }
        crate::fs::vfs::spec::RootSpec::Local(_) => comparable_local_root(raw)
            .map(|root| format!("local:{}:{}", root.prefix, root.segments.join("/")))
            .unwrap_or_else(|| format!("local:{}", raw.trim())),
        crate::fs::vfs::spec::RootSpec::UnknownScheme { raw, .. } => {
            format!("unknown:{raw}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_job() -> Job {
        Job {
            source: "/data/source".into(),
            targets: vec!["/data/target".into()],
            ..Default::default()
        }
    }

    #[test]
    fn persisted_choices_are_closed_vocabularies() {
        let cases = [
            ("mode", "unknown"),
            ("rigor", "typo"),
            ("evidence", "sometimes"),
            ("symlinks", "follow"),
            ("on_conflict", "overwrite"),
        ];
        for (field, value) in cases {
            let mut job = valid_job();
            match field {
                "mode" => job.mode = value.into(),
                "rigor" => job.rigor = value.into(),
                "evidence" => job.evidence = Some(value.into()),
                "symlinks" => job.symlinks = value.into(),
                "on_conflict" => job.on_conflict = value.into(),
                _ => unreachable!(),
            }
            let error = job.validate().unwrap_err();
            assert!(error.contains(field), "{field}: {error}");
        }
    }

    #[test]
    fn numeric_settings_refuse_non_finite_and_out_of_range_values() {
        for (field, value) in [
            ("min_free_pct", f64::NAN),
            ("min_free_pct", -0.1),
            ("min_free_pct", 1.1),
            ("max_delete_ratio", f64::INFINITY),
            ("max_delete_ratio", -0.1),
        ] {
            let mut job = valid_job();
            if field == "min_free_pct" {
                job.min_free_pct = value;
            } else {
                job.max_delete_ratio = value;
            }
            let error = job.validate().unwrap_err();
            assert!(error.contains(field), "{field}: {error}");
        }

        let mut job = valid_job();
        job.max_delete_ratio = 1.1;
        assert!(
            job.validate().is_ok(),
            "a ratio >= 1 disables the deletion gate"
        );
        let mut job = valid_job();
        job.parallel = Some(17);
        assert!(job.validate().unwrap_err().contains("parallel"));
        let mut job = valid_job();
        job.max_conflicts = -2;
        assert!(job.validate().unwrap_err().contains("max_conflicts"));
        let mut job = valid_job();
        job.autoscan_interval_secs = Some(0);
        assert!(job
            .validate()
            .unwrap_err()
            .contains("autoscan_interval_secs"));
        let mut job = valid_job();
        job.autoscan_auto_apply = true;
        assert!(job.validate().unwrap_err().contains("autoscan_auto_apply"));
    }

    #[test]
    fn roots_must_be_present_distinct_and_not_nested() {
        let mut job = valid_job();
        job.targets.clear();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("at least one target root"));

        let mut job = valid_job();
        job.source.clear();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("source root cannot be empty"));

        let mut job = valid_job();
        job.targets[0] = "/data/source".into();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("different directories"));

        let mut job = valid_job();
        job.targets[0] = "/data/source/child".into();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("target cannot be nested"));

        let mut job = valid_job();
        job.source = r"C:\Data\Source".into();
        job.targets[0] = "c:/data/source/child".into();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("target cannot be nested"));

        let mut job = valid_job();
        job.targets = vec!["/data/a".into(), "/data/a".into()];
        assert!(job.validate().unwrap_err().contains("duplicates"));

        let mut job = valid_job();
        job.source = "sftp://user@host/data/source".into();
        job.targets[0] = "sftp://user@HOST:22/data/source/child".into();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("target cannot be nested"));

        let mut job = valid_job();
        job.source = "sftp://host/data".into();
        job.targets[0] = "sftp://host/data/../escape".into();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("cannot contain a '..'"));
    }

    #[test]
    fn single_target_job_binds_one_validated_target_index() {
        let mut job = valid_job();
        job.mode = "mirror".into();
        job.targets.push("/data/backup".into());
        let selected = job.select_target(1).unwrap();
        assert_eq!(selected.target_index(), 1);
        assert_eq!(selected.target(), "/data/backup");
        assert_eq!(selected.configuration().targets, vec!["/data/backup"]);

        let error = job.select_target(2).unwrap_err();
        assert!(error.contains("out of range"));
    }
}
