//! What the job editor needs while a job is being edited: whether the roots look right, what a
//! filter mask would hit, and the junk presets available to tick.

use crate::dto::{EndpointReadiness, JunkPresetDto, PathInfo, PathVerdict};

/// Live health check for the editor: whether the path exists, whether it is a directory, whether it
/// carries a mount-point marker, and how the two roots relate (identical / nested). A mistyped path costs
/// too much to be reported only in the status bar once Compare runs.
#[tauri::command]
pub fn inspect_paths(source: String, target: String) -> PathVerdict {
    fn local_info(raw: &str) -> (PathInfo, Option<String>) {
        let path = std::path::Path::new(raw);
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => {
                let marker = syncdash::pipeline::guard::marker::marker_path(path);
                let (has_marker, warning) = match std::fs::metadata(&marker) {
                    Ok(marker_metadata) => (Some(marker_metadata.is_file()), None),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        (Some(false), None)
                    }
                    Err(error) => (
                        None,
                        Some(format!(
                            "cannot inspect mount marker at {}: {error}",
                            marker.display()
                        )),
                    ),
                };
                (
                    PathInfo {
                        readiness: EndpointReadiness::Ready,
                        exists: Some(true),
                        is_dir: Some(true),
                        has_marker,
                    },
                    warning,
                )
            }
            Ok(_) => (
                PathInfo {
                    readiness: EndpointReadiness::NotDirectory,
                    exists: Some(true),
                    is_dir: Some(false),
                    has_marker: None,
                },
                None,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
                PathInfo {
                    readiness: EndpointReadiness::Missing,
                    exists: Some(false),
                    is_dir: None,
                    has_marker: None,
                },
                None,
            ),
            Err(error) => (
                PathInfo {
                    readiness: EndpointReadiness::Unobservable,
                    exists: None,
                    is_dir: None,
                    has_marker: None,
                },
                Some(format!("cannot inspect local endpoint '{raw}': {error}")),
            ),
        }
    }

    fn info(label: &str, raw: &str) -> (PathInfo, Option<String>, Option<String>) {
        use syncdash::fs::vfs::spec::{parse, RootSpec};
        let raw = raw.trim();
        if raw.is_empty() {
            return (PathInfo::default(), None, None);
        }
        match parse(raw) {
            RootSpec::Local(_) => {
                let (info, warning) = local_info(raw);
                let warning = match info.readiness {
                    EndpointReadiness::Missing => Some(format!(
                        "{label} does not exist: {raw} (Compare will refuse until the endpoint is available)"
                    )),
                    EndpointReadiness::NotDirectory => {
                        Some(format!("{label} is not a directory: {raw}"))
                    }
                    _ => warning.map(|warning| format!("{label}: {warning}")),
                };
                (info, warning, None)
            }
            RootSpec::Remote(remote) if remote.host.trim().is_empty() => (
                PathInfo {
                    readiness: EndpointReadiness::Invalid,
                    exists: None,
                    is_dir: None,
                    has_marker: None,
                },
                Some(format!("{label}: {}:// root has no host", remote.scheme)),
                None,
            ),
            RootSpec::Remote(remote) => (
                PathInfo {
                    readiness: EndpointReadiness::Deferred,
                    exists: None,
                    is_dir: None,
                    has_marker: None,
                },
                None,
                Some(format!(
                    "{label}: {}:// readiness is not probed in the editor; Compare connects with the configured credentials and validates it",
                    remote.scheme
                )),
            ),
            RootSpec::UnknownScheme { scheme, .. } => (
                PathInfo {
                    readiness: EndpointReadiness::Invalid,
                    exists: None,
                    is_dir: None,
                    has_marker: None,
                },
                Some(format!(
                    "{label}: unknown scheme '{scheme}://' — this will be refused, never treated as a local path"
                )),
                None,
            ),
        }
    }

    let (source_info, source_warning, source_note) = info("source", &source);
    let (target_info, target_warning, target_note) = info("target", &target);
    let mut v = PathVerdict {
        source: source_info,
        target: target_info,
        warnings: source_warning.into_iter().chain(target_warning).collect(),
        notes: source_note.into_iter().chain(target_note).collect(),
    };
    let (s, t) = (source.trim(), target.trim());
    let invalid_phrase = matches!(v.source.readiness, EndpointReadiness::Invalid)
        || matches!(v.target.readiness, EndpointReadiness::Invalid);
    if !invalid_phrase && !s.is_empty() && !t.is_empty() {
        if let Err(reason) = syncdash::job::validate_root_pair(s, t) {
            if !v.warnings.contains(&reason) {
                v.warnings.push(reason);
            }
        }
    }
    v
}

/// Ad-hoc mask matching for Advanced Filters. The frontend **does not write its own glob** — the FFS mask
/// semantics have exactly one implementation, in filter.rs, so a mask tried out in the UI behaves identically once written into the job's exclude.
#[tauri::command]
pub fn mask_match(masks: Vec<String>, paths: Vec<String>) -> Vec<bool> {
    syncdash::pipeline::filter::mask_hits(&masks, &paths)
}

/// The junk presets, patterns and all. The frontend **does not carry its own copy of these lists** —
/// the editor's checkboxes write literally what the engine would have applied, which is the whole
/// reason a checkbox can now be trusted to describe what a job excludes.
#[tauri::command]
pub fn junk_presets() -> Vec<JunkPresetDto> {
    syncdash::job::junk::JUNK_PRESETS
        .iter()
        .map(|p| JunkPresetDto {
            id: p.id.to_string(),
            label: p.label.to_string(),
            hint: p.hint.to_string(),
            patterns: p.patterns.iter().map(|s| s.to_string()).collect(),
            default_on: p.default_on,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_targets_are_not_promised_to_be_created() {
        let verdict = inspect_paths(
            "/definitely/missing/source".into(),
            "/definitely/missing/target".into(),
        );
        let message = verdict.warnings.join("\n");
        assert_eq!(verdict.source.readiness, EndpointReadiness::Missing);
        assert_eq!(verdict.target.readiness, EndpointReadiness::Missing);
        assert_eq!(verdict.source.exists, Some(false));
        assert!(message.contains("Compare will refuse"), "{message}");
        assert!(!message.contains("will be created"), "{message}");
    }

    #[test]
    fn network_readiness_is_explicitly_deferred_not_faked_as_a_directory() {
        let verdict = inspect_paths("sftp://user@host/data".into(), "smb://server/share".into());
        assert_eq!(verdict.source.readiness, EndpointReadiness::Deferred);
        assert_eq!(verdict.target.readiness, EndpointReadiness::Deferred);
        assert_eq!(verdict.source.exists, None);
        assert_eq!(verdict.source.is_dir, None);
        assert_eq!(verdict.source.has_marker, None);
        assert_eq!(verdict.notes.len(), 2);
        assert!(verdict.notes.iter().all(|note| note.contains("not probed")));
    }

    #[test]
    fn unknown_schemes_are_invalid_not_local_or_deferred() {
        let verdict = inspect_paths("sfpt://host/data".into(), String::new());
        assert_eq!(verdict.source.readiness, EndpointReadiness::Invalid);
        assert_eq!(verdict.source.exists, None);
        assert!(verdict.warnings.join("\n").contains("unknown scheme"));
    }

    #[test]
    fn a_local_file_is_observed_as_present_but_not_a_directory() {
        let path = std::env::temp_dir().join(format!(
            "syncdash-endpoint-file-{}-{}",
            std::process::id(),
            syncdash::foundation::time::now_ms()
        ));
        std::fs::write(&path, b"not a root").unwrap();
        let verdict = inspect_paths(path.to_string_lossy().into_owned(), String::new());
        let _ = std::fs::remove_file(path);

        assert_eq!(verdict.source.readiness, EndpointReadiness::NotDirectory);
        assert_eq!(verdict.source.exists, Some(true));
        assert_eq!(verdict.source.is_dir, Some(false));
        assert!(verdict.warnings.join("\n").contains("not a directory"));
    }

    #[test]
    fn remote_nested_roots_use_engine_validation() {
        let verdict = inspect_paths(
            "sftp://user@host/data".into(),
            "sftp://user@HOST:22/data/child".into(),
        );
        assert!(
            verdict
                .warnings
                .iter()
                .any(|warning| warning.contains("target cannot be nested")),
            "{:?}",
            verdict.warnings
        );
    }
}
