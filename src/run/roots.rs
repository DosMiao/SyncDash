//! Turning a root phrase into an open backend.
//!
//! Every entry point resolves both roots before doing anything else, so an auth failure or an
//! unreachable host surfaces here rather than half way through a scan.



/// Open + connect a root phrase to a live backend (a plain local path resolves to `LocalVfs`).
///
/// Local paths pass through untouched. A backend that *translates* to a local path (`smb://` → UNC
/// or mount point) connects first — that is where mount orchestration lives — and hands back the
/// translated path. A `peer://` phrase never reaches here: that root belongs to the far side's own
/// syncdash, so `vfs::open` refuses it rather than opening something local that looks similar.
pub fn resolve_root(s: &str) -> std::io::Result<std::sync::Arc<dyn crate::fs::vfs::Vfs>> {
    let v = crate::fs::vfs::open(s, &crate::fs::vfs::cred::default_provider())?;
    v.connect().map_err(std::io::Error::from)?;
    Ok(v)
}

// ---- transport router ----
//
// A job either runs here or runs over ssh on a peer, and every entry point has to make that
// choice. It used to be made at each call site: `if job.remote_host.is_some()` appeared six times
// across the two shells, and the two of them had already drifted — the CLI passed `accept_caps`
// only on the local branch, the desktop re-derived the run-log kind string with its own copy of
// the same condition. `run::mod` now has the only four.
//
// What that condition reads changed too. `remote_host` was a second place to say where a root
// lives, alongside the phrase that already said it, and `validate_roots` carried a rule whose only
// purpose was to stop the two from contradicting each other. The answer is the phrase — a
// `peer://` target — and that rule is gone with the field.
