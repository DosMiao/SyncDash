//! Root reachability: does it exist, is it a directory, and does it carry its marker.

use std::path::Path;

use super::marker::has_marker;
use super::Verdict;
use crate::foundation::names::MARKER_NAME;

/// Root availability + marker check. `label` is only used in messages ("source"/"target").
pub fn check_root(label: &str, root: &Path, require_marker: bool, v: &mut Verdict) {
    if !root.is_dir() {
        v.blockers.push(format!("{label} root not accessible: {}", root.display()));
        return;
    }
    if require_marker && !has_marker(root) {
        v.blockers.push(format!(
            "{label} root has no {MARKER_NAME} marker: {} \
             — the share may not be mounted. Run `syncdash mark <root>` once on the real data, \
             or set require_marker = false in the job.",
            root.display()
        ));
        return;
    }
    // Even without a required marker, an empty directory plus planned deletions is worth a warning (decided in check_plan)
    if !require_marker && !has_marker(root) {
        let empty = std::fs::read_dir(root).map(|mut d| d.next().is_none()).unwrap_or(false);
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
pub fn check_root_vfs(label: &str, vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>, require_marker: bool, v: &mut Verdict) {
    use crate::model::table::EntryKind;
    let disp = vfs.display();
    match vfs.stat("") {
        Ok(Some(m)) if m.kind == EntryKind::Dir => {}
        Ok(Some(_)) => {
            v.blockers.push(format!("{label} root is not a directory: {disp}"));
            return;
        }
        Ok(None) => {
            v.blockers.push(format!("{label} root does not exist: {disp}"));
            return;
        }
        Err(e) => {
            v.blockers.push(format!("{label} root not accessible: {disp} ({e})"));
            return;
        }
    }
    let marker = matches!(vfs.stat(MARKER_NAME), Ok(Some(_)));
    if require_marker && !marker {
        v.blockers.push(format!(
            "{label} root has no {MARKER_NAME} marker: {disp} \
             — the share may not be mounted. Run `syncdash mark <root>` once on the real data, \
             or set require_marker = false in the job."
        ));
        return;
    }
    if !require_marker && !marker {
        let empty = vfs.read_dir_names("").map(|l| l.is_empty()).unwrap_or(false);
        if empty {
            v.warnings.push(format!(
                "{label} root is empty and unmarked: {disp} — if this root simply isn't reachable, \
                 stop now (enable require_marker to make this an error)"
            ));
        }
    }
}

// ---- the capability report: the no-silent rule, mechanized ----

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::marker::write_marker;
    use super::super::Verdict;

    #[test]
    fn missing_marker_blocks_when_required() {
        let d = std::env::temp_dir().join(format!("syncdash-pf-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let mut v = Verdict { blockers: vec![], warnings: vec![] };
        check_root("target", &d, true, &mut v);
        assert_eq!(v.blockers.len(), 1);

        write_marker(&d, "test-job", "").unwrap();
        let mut v2 = Verdict { blockers: vec![], warnings: vec![] };
        check_root("target", &d, true, &mut v2);
        assert!(v2.ok(), "marker present -> pass");
        let _ = std::fs::remove_dir_all(&d);
    }
}
