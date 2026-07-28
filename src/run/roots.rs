//! Turning a root phrase into an open backend.
//!
//! Every entry point resolves both roots before doing anything else, so an auth failure or an
//! unreachable host surfaces here rather than half way through a scan.



/// Resolve one root phrase to something the engine's local lanes can touch.
/// Local paths pass through untouched. A backend that *translates* to a local path
/// (smb:// → UNC / mount point) connects first — that is where mount orchestration
/// lives — and hands back the translated path. A genuinely remote backend has no
/// local path; until its engine lane exists (scan M3 / apply M4) that is a loud
/// error naming the milestone, never a silent fallback.
/// Open + connect a root phrase to a live backend (a plain local path resolves to LocalVfs).
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
// the same condition. These four are the only places that branch now.
