//! Turning a root phrase into an open backend.
//!
//! Every entry point resolves both roots before doing anything else, so an auth failure or an
//! unreachable host surfaces here rather than half way through a scan.

/// Open + connect a root phrase to a live backend (a plain local path resolves to `LocalVfs`).
///
/// Connecting here rather than lazily is the point: a protocol root's authentication and reachability
/// both resolve at this line, so a bad credential surfaces before a scan starts rather than part way
/// through one. A `peer://` phrase never reaches here — that root belongs to the far side's own
/// syncdash, so `vfs::open` refuses it rather than opening something local that looks similar.
pub fn resolve_root(s: &str) -> std::io::Result<std::sync::Arc<dyn crate::fs::vfs::Vfs>> {
    let v = crate::fs::vfs::open(s)?;
    v.connect().map_err(std::io::Error::from)?;
    Ok(v)
}
