/// Peer connection parameters produced by a live probe.
///
/// Desktop Compare and Apply are independent IPC rounds, so Apply probes again instead of relying
/// on a connection retained from Compare.
pub struct PeerLink {
    pub host: String,
    pub executable: String,
    pub peer_root: String,
    /// A local path serving the *same* tree the peer syncs — the `|mount=` option.
    ///
    /// The peer lane pushes: it packs the target-side ops and the far side applies them. The
    /// reverse (source-side) direction has nothing to push, so it writes through this mount
    /// instead. It is an option on the phrase rather than an assumption because a peer job used
    /// to depend on it silently: the mount lived in `target` alongside an unrelated
    /// `remote_root`, nothing said the two named one tree, and a missing mount skipped those ops
    /// with a warning nobody had a reason to expect.
    pub mount: Option<std::path::PathBuf>,
    pub shell: crate::transfer::peer::PeerShell,
    /// The live ssh session, held for the whole stage. The old transport handshook once per
    /// command; a compare stage runs several.
    pub session: crate::transfer::peer::PeerSession,
}

impl PeerLink {
    /// Build one syncdash command line for this peer's shell dialect.
    pub(super) fn command(&self, arguments: &[String]) -> String {
        crate::transfer::peer::peer_command(self.shell, &self.executable, arguments)
    }
}
