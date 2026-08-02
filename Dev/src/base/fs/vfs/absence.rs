//! Confirming that a name is really gone, rather than momentarily unreachable.
//!
//! This is the single failure mode `lib.rs` names as the one this tool exists not to have. A
//! protocol backend that answers "no such file" during a network hiccup, and is believed, produces
//! a snapshot with the entry missing — and the next Compare reads that as a deletion the user made
//! and mirrors it. The FFS defense is to treat a not-found answer as a *claim* and verify it: list
//! the parent, and only accept the absence if the parent really does not carry the name.
//!
//! The rule was implemented twice, in the SMB and SFTP backends, with byte-identical user-facing
//! wording. Two copies of a three-branch rule is two chances for one of them to start believing a
//! server, so the branches live here and each backend supplies only its own listing call.

use super::error::{VfsError, VfsErrorKind, VfsResult};

/// Verify that `rel` is absent, given a way to list its parent directory.
///
/// The three branches and why each is what it is:
///
/// - the parent lists the name → the server contradicted itself; `Transient`, never absence
/// - the parent is itself `NotFound` → the absence stands on its own, nothing left to check
/// - the listing failed for any other reason → `Transient`, because an absence that cannot be
///   confirmed is not an absence
///
/// Note that only the second branch returns `Ok`. Everything ambiguous resolves to `Transient`,
/// which is the asymmetry the error taxonomy exists to preserve.
pub(super) fn confirm_absent(
    rel: &str,
    list_parent: impl FnOnce(&str) -> VfsResult<Vec<String>>,
) -> VfsResult<()> {
    let (parent, name) = crate::foundation::path::split_parent(rel);
    let parent = parent.trim_end_matches('/');
    match list_parent(parent) {
        Ok(names) => {
            if names.iter().any(|entry| entry == name) {
                return Err(VfsError::new(
                    VfsErrorKind::Transient,
                    format!(
                        "the server reported '{rel}' missing but its parent still lists it — treating as a temporary fault, not a deletion"
                    ),
                ));
            }
            Ok(())
        }
        // Parent gone too: the absence stands on its own.
        Err(error) if error.kind == VfsErrorKind::NotFound => Ok(()),
        Err(error) => Err(VfsError::new(
            VfsErrorKind::Transient,
            format!("cannot confirm '{rel}' is really absent (parent listing failed: {error})"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn not_found() -> VfsError {
        VfsError::new(VfsErrorKind::NotFound, "no such file".to_string())
    }

    #[test]
    fn a_parent_that_still_lists_the_name_makes_the_absence_transient() {
        let error = confirm_absent("dir/file.txt", |parent| {
            assert_eq!(parent, "dir");
            Ok(vec!["file.txt".to_string(), "other".to_string()])
        })
        .expect_err("a contradicted absence must not be believed");
        assert_eq!(error.kind, VfsErrorKind::Transient);
    }

    #[test]
    fn a_parent_without_the_name_confirms_the_absence() {
        assert!(confirm_absent("dir/file.txt", |_| Ok(vec!["other".to_string()])).is_ok());
    }

    #[test]
    fn a_missing_parent_lets_the_absence_stand() {
        assert!(confirm_absent("dir/file.txt", |_| Err(not_found())).is_ok());
    }

    #[test]
    fn an_unobtainable_listing_is_transient_not_absence() {
        let error = confirm_absent("dir/file.txt", |_| {
            Err(VfsError::new(
                VfsErrorKind::Transient,
                "connection reset".to_string(),
            ))
        })
        .expect_err("an absence that cannot be confirmed is not an absence");
        assert_eq!(error.kind, VfsErrorKind::Transient);
    }

    #[test]
    fn a_top_level_name_checks_the_root() {
        assert!(confirm_absent("file.txt", |parent| {
            assert_eq!(parent, "", "a top-level name lists the root");
            Ok(Vec::new())
        })
        .is_ok());
    }
}
