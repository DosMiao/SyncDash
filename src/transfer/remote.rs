//! ssh transport for the peer lane, in-process on russh.
//!
//! It used to spawn `ssh -o BatchMode=yes`, which meant SyncDash could only reach a peer on a
//! machine with OpenSSH installed, could not measure a transfer while it ran, and could not stop
//! one: a cancelled run left the child copying until it finished on its own. The session is
//! `fs::ssh`'s, the same one `sftp://` uses — one auth chain, one `known_hosts` check.
//!
//! **Being in-process removes the child process, not the shell.** SSH's exec request (RFC 4254
//! §6.5) carries a single command string that sshd hands to the user's login shell, so the far
//! side still parses what we send. `RemoteShell` and the quoting below therefore stay: OpenSSH on
//! Windows defaults to PowerShell, whose quoting rules differ from POSIX, and the `chcp 65001`
//! prelude is what keeps non-ASCII paths off the OEM code page.

use std::io::{Error, ErrorKind, Read};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use russh::ChannelMsg;

use crate::fs::ssh;
use crate::fs::vfs::cred::default_provider;
use crate::fs::vfs::spec::{parse, RootSpec};

/// Wrap in POSIX single quotes (an embedded single quote is escaped as '\'')
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Remote shell dialect: OpenSSH on Windows defaults to PowerShell
#[derive(Clone, Copy, PartialEq)]
pub enum RemoteShell {
    Posix,
    PowerShell,
}

impl RemoteShell {
    pub fn from_os(os: &str) -> RemoteShell {
        if os == "windows" { RemoteShell::PowerShell } else { RemoteShell::Posix }
    }
}

/// Quote for the remote dialect (inside PowerShell single quotes, an embedded single quote is written '')
fn shq_for(shell: RemoteShell, s: &str) -> String {
    match shell {
        RemoteShell::Posix => shq(s),
        RemoteShell::PowerShell => format!("'{}'", s.replace('\'', "''")),
    }
}

/// Assemble the remote syncdash invocation:
/// - posix: exe left bare (so `~` expands), every argument through shq
/// - powershell: `chcp 65001 >$null; & '<exe>' args...` (switch to UTF-8 first, or non-ASCII paths get mangled by the OEM code page)
pub fn remote_cmd(shell: RemoteShell, exe: &str, args: &[String]) -> String {
    match shell {
        RemoteShell::Posix => {
            let mut c = exe.to_string();
            for a in args {
                c.push(' ');
                c.push_str(&shq(a));
            }
            c
        }
        RemoteShell::PowerShell => {
            let mut c = format!("chcp 65001 >$null; & {}", shq_for(shell, exe));
            for a in args {
                c.push(' ');
                c.push_str(&shq_for(shell, a));
            }
            c
        }
    }
}

/// A live session to one peer.
///
/// Held open across the calls of a single stage rather than reconnecting per command: the old
/// transport paid a full handshake for every `ssh` invocation, and a compare stage makes several.
pub struct PeerSession {
    rt: tokio::runtime::Runtime,
    handle: russh::client::Handle<ssh::HostCheck>,
    host: String,
    timeout: Duration,
}

/// Where a peer is and how to reach it, read off a `peer://` phrase.
pub struct PeerAddr {
    pub host: String,
    pub port: u16,
    pub user: String,
}

/// `peer://[user@]host[:port]/…` → the address to dial. The user defaults to this machine's
/// login name, matching what `ssh host` would have done.
pub fn addr_of(phrase: &str) -> std::io::Result<PeerAddr> {
    let RootSpec::Remote(r) = parse(phrase) else {
        return Err(Error::new(ErrorKind::InvalidInput, format!("'{phrase}' is not a peer:// root")));
    };
    Ok(PeerAddr {
        host: r.host.clone(),
        port: r.port.unwrap_or(22),
        user: r.user.clone().unwrap_or_else(|| {
            std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default()
        }),
    })
}

impl PeerSession {
    /// Dial and authenticate. `phrase` is the `peer://` root, used for the address and for naming
    /// the root in any error the handshake raises.
    pub fn open(phrase: &str) -> std::io::Result<PeerSession> {
        let a = addr_of(phrase)?;
        let timeout = Duration::from_secs(20);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_io()
            .enable_time()
            .thread_name("syncdash-peer")
            .build()?;
        let creds = default_provider()
            .credentials_for(&match parse(phrase) {
                RootSpec::Remote(r) => r,
                _ => unreachable!("addr_of already rejected a non-remote phrase"),
            })
            .map_err(std::io::Error::from)?;
        let session = rt
            .block_on(async { ssh::connect(&a.host, a.port, &a.user, &creds, timeout, phrase).await })
            .map_err(std::io::Error::from)?;
        Ok(PeerSession { rt, handle: session.handle, host: a.host, timeout })
    }

    /// Run one command, collecting stdout. Stderr is forwarded to this process's own stderr, which
    /// is where the far side's diagnostics used to land when `ssh` inherited it.
    pub fn capture(&self, cmd: &str) -> std::io::Result<Vec<u8>> {
        let (out, code) = self.run(cmd, None)?;
        if code != 0 {
            return Err(Error::other(format!(
                "ssh command failed on {} (exit {code}): {cmd}",
                self.host
            )));
        }
        Ok(out)
    }

    /// Run one command for its exit status alone.
    pub fn run_status(&self, cmd: &str) -> std::io::Result<bool> {
        Ok(self.run(cmd, None)?.1 == 0)
    }

    /// Run one command with a local file streamed to its stdin.
    ///
    /// `on_chunk` is called with each block's size as it leaves, and its `Err` stops the transfer
    /// — which is the other half of being in-process. The old transport handed the file to a child
    /// and learned nothing until it exited: a multi-gigabyte package showed no movement, and a
    /// cancelled run kept uploading until it finished on its own.
    pub fn send_file(
        &self,
        cmd: &str,
        file: &Path,
        on_chunk: &mut dyn FnMut(u64) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let f = std::fs::File::open(file)?;
        let (_, code) = self.run(cmd, Some((f, on_chunk)))?;
        if code != 0 {
            return Err(Error::other(format!(
                "stdin-ship to {} failed (exit {code}): {cmd}",
                self.host
            )));
        }
        Ok(())
    }

    /// The one place a channel is opened, driven and closed.
    fn run(
        &self,
        cmd: &str,
        mut stdin: Option<(std::fs::File, &mut dyn FnMut(u64) -> std::io::Result<()>)>,
    ) -> std::io::Result<(Vec<u8>, u32)> {
        self.rt.block_on(async {
            let mut ch = self
                .handle
                .channel_open_session()
                .await
                .map_err(|e| Error::other(format!("cannot open a channel to {}: {e}", self.host)))?;
            ch.exec(true, cmd)
                .await
                .map_err(|e| Error::other(format!("exec refused by {}: {e}", self.host)))?;

            if let Some((mut f, on_chunk)) = stdin.take() {
                // 256 KiB: large enough that the per-chunk window update is not the bottleneck,
                // small enough that a cancel between chunks is felt quickly.
                let mut buf = vec![0u8; 256 * 1024];
                loop {
                    let n = f.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    ch.data(&buf[..n])
                        .await
                        .map_err(|e| Error::other(format!("sending to {} failed: {e}", self.host)))?;
                    // A cancel here abandons the channel: dropping it without eof tells the far
                    // side's `recv` that the stream ended early, so it fails its hash check and
                    // writes nothing. Half a package can never be applied.
                    on_chunk(n as u64)?;
                }
                // The far side reads stdin to EOF; without this it waits forever.
                ch.eof().await.map_err(|e| Error::other(format!("eof to {} failed: {e}", self.host)))?;
            }

            let mut out = Vec::new();
            let mut code: Option<u32> = None;
            loop {
                let Ok(msg) = tokio::time::timeout(self.timeout, ch.wait()).await else {
                    return Err(Error::other(format!(
                        "{} went quiet for {:?} during: {cmd}",
                        self.host, self.timeout
                    )));
                };
                match msg {
                    Some(ChannelMsg::Data { ref data }) => out.extend_from_slice(data),
                    // ext 1 is stderr; it goes where it went before — this process's stderr
                    Some(ChannelMsg::ExtendedData { ref data, ext: 1 }) => {
                        use std::io::Write as _;
                        let _ = std::io::stderr().write_all(data);
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => code = Some(exit_status),
                    Some(_) => {}
                    // Channel closed. No exit status means the far side died without reporting
                    // one; that is a failure, not a success with no news.
                    None => break,
                }
            }
            Ok((out, code.unwrap_or(255)))
        })
    }
}

/// Shared handle so a stage can pass one session down without threading a lifetime.
pub type SharedPeer = Arc<PeerSession>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Moving in-process removed the child process, not the shell: sshd still hands the command
    /// string to the far side's login shell, so both dialects still have to be quoted for.
    #[test]
    fn posix_quoting_survives_an_embedded_quote() {
        let c = remote_cmd(RemoteShell::Posix, "syncdash", &["scan".into(), "/it's/here".into()]);
        assert_eq!(c, r#"syncdash 'scan' '/it'\''s/here'"#);
        // The exe stays bare so the remote shell still expands a leading ~
        assert!(remote_cmd(RemoteShell::Posix, "~/bin/syncdash", &[]).starts_with("~/bin/syncdash"));
    }

    #[test]
    fn powershell_doubles_quotes_and_switches_the_code_page_first() {
        let c = remote_cmd(RemoteShell::PowerShell, "sd.exe", &["scan".into(), "C:\\it's".into()]);
        // chcp 65001 or a non-ASCII path arrives mangled by the OEM code page
        assert!(c.starts_with("chcp 65001 >$null; & 'sd.exe'"), "{c}");
        assert!(c.ends_with("'C:\\it''s'"), "{c}");
    }

    #[test]
    fn the_dialect_follows_the_probed_os() {
        assert!(RemoteShell::from_os("windows") == RemoteShell::PowerShell);
        assert!(RemoteShell::from_os("macos") == RemoteShell::Posix);
        assert!(RemoteShell::from_os("") == RemoteShell::Posix, "unknown means posix, not a panic");
    }

    #[test]
    fn a_peer_phrase_resolves_to_an_address() {
        let a = addr_of("peer://mac/Users/ben/x").unwrap();
        assert_eq!(a.host, "mac");
        assert_eq!(a.port, 22, "the ssh default, same as `ssh mac` would have used");
        let a = addr_of("peer://ben@mac:2222/Users/ben/x").unwrap();
        assert_eq!((a.host.as_str(), a.port, a.user.as_str()), ("mac", 2222, "ben"));
        // A non-peer phrase is a caller error, not something to dial anyway
        assert!(addr_of(r"D:\Code\x").is_err());
    }
}
