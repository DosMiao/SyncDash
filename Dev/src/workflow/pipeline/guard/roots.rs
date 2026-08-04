//! Root reachability: does it exist, is it a directory, and does it carry its marker.

use std::path::Path;

use super::marker::{inspect_marker_in, Marker};
use super::Verdict;
use crate::foundation::names::MARKER_NAME;
use crate::foundation::path::RootRelativeDir;
use crate::fs::local_root::LocalRoot;

/// Root availability + marker check. `label` is only used in messages ("source"/"target").
pub fn check_root(label: &str, root: &Path, require_marker: bool, v: &mut Verdict) {
    let local_root = match LocalRoot::open(root.to_path_buf()) {
        Ok(root) => root,
        Err(error) => {
            v.blockers.push(format!(
                "{label} root not accessible: {} ({error})",
                root.display()
            ));
            return;
        }
    };
    let marker = match inspect_marker_in(&local_root) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => {
            let message = format!(
                "{label} root has an invalid {MARKER_NAME} marker: {} ({error})",
                root.display()
            );
            if require_marker {
                v.blockers.push(message);
                return;
            }
            v.warnings.push(message);
            false
        }
    };
    if require_marker && !marker {
        v.blockers.push(format!(
            "{label} root has no {MARKER_NAME} marker: {} \
             — the share may not be mounted. Run `syncdash mark <root>` once on the real data, \
             or set require_marker = false in the job.",
            root.display()
        ));
        return;
    }
    // Even without a required marker, an empty directory plus planned deletions is worth a warning (decided in check_plan)
    if !require_marker && !marker {
        let empty = local_root
            .read_directory(&RootRelativeDir::new("").expect("empty path names the root"))
            .map(|listing| listing.entries.is_empty())
            .unwrap_or(false);
        if empty {
            v.warnings.push(format!(
                "{label} root is empty and unmarked: {} — if this share simply isn't mounted, \
                 stop now (enable require_marker to make this an error)",
                root.display()
            ));
        }
    }
}

/// The same three-level judgment for a VFS root (syncthing's folder-marker defense):
/// unreachable / not-a-directory → blocker; marker demanded but absent → blocker;
/// empty and unmarked → warning. Distinguishing "the mount is not up" from "the user
/// deleted everything" is the whole game on a network root.
pub fn check_root_vfs(
    label: &str,
    vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    require_marker: bool,
    v: &mut Verdict,
) {
    use crate::fs::vfs::VfsEntryKind;
    let disp = vfs.display();
    match vfs.stat("") {
        Ok(Some(m)) if m.kind == VfsEntryKind::Directory => {}
        Ok(Some(_)) => {
            v.blockers
                .push(format!("{label} root is not a directory: {disp}"));
            return;
        }
        Ok(None) => {
            v.blockers
                .push(format!("{label} root does not exist: {disp}"));
            return;
        }
        Err(e) => {
            v.blockers
                .push(format!("{label} root not accessible: {disp} ({e})"));
            return;
        }
    }
    let marker = match inspect_marker_vfs(vfs) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => {
            let message =
                format!("{label} root has an invalid {MARKER_NAME} marker: {disp} ({error})");
            if require_marker {
                v.blockers.push(message);
                return;
            }
            v.warnings.push(message);
            false
        }
    };
    if require_marker && !marker {
        v.blockers.push(format!(
            "{label} root has no {MARKER_NAME} marker: {disp} \
             — the share may not be mounted. Run `syncdash mark <root>` once on the real data, \
             or set require_marker = false in the job."
        ));
        return;
    }
    if !require_marker && !marker {
        let empty = vfs
            .read_dir_names("")
            .map(|l| l.is_empty())
            .unwrap_or(false);
        if empty {
            v.warnings.push(format!(
                "{label} root is empty and unmarked: {disp} — if this root simply isn't reachable, \
                 stop now (enable require_marker to make this an error)"
            ));
        }
    }
}

fn inspect_marker_vfs(
    vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
) -> std::io::Result<Option<Marker>> {
    use crate::fs::vfs::VfsEntryKind;
    use std::io::Read as _;

    match vfs.stat(MARKER_NAME).map_err(std::io::Error::from)? {
        None => return Ok(None),
        Some(metadata) if metadata.kind == VfsEntryKind::File => {}
        Some(metadata) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("marker is a {:?}, not a regular file", metadata.kind),
            ))
        }
    }
    let mut stream = vfs.open_read(MARKER_NAME).map_err(std::io::Error::from)?;
    let mut text = String::new();
    stream.read_to_string(&mut text)?;
    serde_json::from_str(&text).map(Some).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("marker JSON is invalid: {error}"),
        )
    })
}

// ---- the capability report: the no-silent rule, mechanized ----

#[cfg(test)]
mod tests {
    use super::super::marker::write_marker;
    use super::super::Verdict;
    use super::*;

    #[test]
    fn missing_marker_blocks_when_required() {
        let d = std::env::temp_dir().join(format!("syncdash-pf-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let mut v = Verdict::default();
        check_root("target", &d, true, &mut v);
        assert_eq!(v.blockers.len(), 1);

        write_marker(&d, "test-job", "").unwrap();
        let mut v2 = Verdict::default();
        check_root("target", &d, true, &mut v2);
        assert!(v2.ok(), "marker present -> pass");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn malformed_marker_does_not_satisfy_the_mount_guard() {
        let d =
            std::env::temp_dir().join(format!("syncdash-pf-invalid-marker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(MARKER_NAME), b"not-json").unwrap();
        let mut verdict = Verdict::default();

        check_root("target", &d, true, &mut verdict);

        assert_eq!(verdict.blockers.len(), 1);
        assert!(verdict.blockers[0].contains("invalid .syncdash-root marker"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
