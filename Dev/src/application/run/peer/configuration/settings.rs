use crate::job::SingleTargetJob;

pub(in crate::run::peer) struct PeerLinkSettings {
    pub(in crate::run::peer) host: String,
    pub(in crate::run::peer) executable: String,
    pub(in crate::run::peer) peer_root: String,
    pub(in crate::run::peer) mount: Option<std::path::PathBuf>,
}

/// Restore a peer root to the absolute path the far side will resolve.
///
/// The phrase grammar strips the leading `/` — right for `sftp://` and `smb://`, where the root is
/// a segment inside a session or a share, and wrong here: a peer root is a path on the far
/// machine's own filesystem and the far syncdash resolves it against *its* working directory. Sent
/// as `Users/ben/x` it lands at `~/Users/ben/x`, which is a path that generally does not exist —
/// so the run reads an empty tree and mirror proposes deleting everything in the source.
///
/// A drive letter is already absolute (a Windows peer takes `C:\…` verbatim); everything else lost
/// a `/` on the way in and gets it back.
pub(in crate::run::peer) fn absolute_peer_root(root: &str) -> String {
    let b = root.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        root.to_string()
    } else {
        format!("/{root}")
    }
}

pub(in crate::run::peer) fn parse_peer_link_settings(
    job: &SingleTargetJob,
) -> std::io::Result<PeerLinkSettings> {
    use crate::fs::vfs::spec::{parse, RootSpec};
    let invalid_input =
        |message: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, message);
    let RootSpec::Endpoint(peer_spec) = parse(job.target()) else {
        return Err(invalid_input(format!(
            "target '{}' is not a peer:// root",
            job.target()
        )));
    };
    if peer_spec.root.is_empty() {
        return Err(invalid_input(format!(
            "target '{}' names no path on {} — a peer root needs one (peer://{}/path/to/tree)",
            job.target(),
            peer_spec.host,
            peer_spec.host
        )));
    }
    Ok(PeerLinkSettings {
        host: peer_spec.host.clone(),
        executable: peer_spec
            .opt("exe")
            .filter(|e| !e.is_empty())
            .unwrap_or("syncdash")
            .to_string(),
        peer_root: absolute_peer_root(&peer_spec.root),
        mount: crate::run::peer_pull_mount(job.configuration()),
    })
}
