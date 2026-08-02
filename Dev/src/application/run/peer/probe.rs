use crate::job::SingleTargetJob;

use super::configuration::parse_peer_link_settings;
use super::link::PeerLink;

/// Open a peer session, validate its reported table schema, and derive its shell dialect.
pub fn probe_peer(
    name: &str,
    job: &SingleTargetJob,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<PeerLink> {
    let settings = parse_peer_link_settings(job)?;
    let host = settings.host.as_str();
    let executable = settings.executable.as_str();
    let session = crate::transfer::peer::PeerSession::open(job.target())?;
    let probe = session.capture(&format!("{executable} probe"))?;
    let pv: serde_json::Value = serde_json::from_slice(&probe).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bad probe output: {e}"),
        )
    })?;
    if pv["schema"].as_u64() != Some(crate::model::table::TABLE_SCHEMA as u64) {
        ctx.log(
            crate::model::event::LogLevel::Warn,
            "peer",
            format!(
                "[{name}] warning: peer schema {} != local {} — rebuild the peer binary",
                pv["schema"],
                crate::model::table::TABLE_SCHEMA
            ),
        );
    }
    let peer_os = pv["os"].as_str().unwrap_or("").to_string();
    ctx.log(
        crate::model::event::LogLevel::Info,
        "peer",
        format!(
            "[{name}] peer {}: {} {}",
            host,
            peer_os,
            pv["arch"].as_str().unwrap_or("?")
        ),
    );
    Ok(PeerLink {
        host: settings.host,
        executable: settings.executable,
        peer_root: settings.peer_root,
        mount: settings.mount,
        shell: crate::transfer::peer::PeerShell::from_os(&peer_os),
        session,
    })
}
