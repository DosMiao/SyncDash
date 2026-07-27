//! ssh transport primitives (v0.6). Everything goes through `ssh -o BatchMode=yes` (key auth is already set up; a password prompt fails outright instead of hanging).
//! Processes are spawned straight from Rust, sidestepping PowerShell 5.1's nested-quoting hell.
//! Remote commands are quoted for the dialect the far side actually runs: `RemoteShell::from_os`
//! picks PowerShell for a Windows remote (doubling embedded single quotes) and POSIX rules otherwise.
//! Windows-as-remote landed in v0.7; the `chcp 65001` prelude there keeps non-ASCII paths from being
//! mangled by the OEM code page.

use std::io::{Error, ErrorKind};
use std::path::Path;
use std::process::{Command, Stdio};

fn ssh_base(host: &str) -> Command {
    let mut c = Command::new("ssh");
    c.args(["-o", "BatchMode=yes", host]);
    c
}

/// Wrap in POSIX single quotes (an embedded single quote is escaped as '\'')
pub fn shq(s: &str) -> String {
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
pub fn shq_for(shell: RemoteShell, s: &str) -> String {
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

/// Run remotely and capture stdout (stderr passes straight through to the local terminal)
pub fn ssh_capture(host: &str, cmd: &str) -> std::io::Result<Vec<u8>> {
    let out = ssh_base(host).arg(cmd).stderr(Stdio::inherit()).output()?;
    if !out.status.success() {
        return Err(Error::new(ErrorKind::Other, format!("ssh command failed on {host}: {cmd}")));
    }
    Ok(out.stdout)
}

/// Run remotely with stdout/stderr passed straight through. Returns whether it succeeded.
pub fn ssh_run(host: &str, cmd: &str) -> std::io::Result<bool> {
    let status = ssh_base(host).arg(cmd).status()?;
    Ok(status.success())
}

/// Run a remote command with a local file wired to its stdin (pairs with the remote `syncdash recv`:
/// Rust reads raw bytes, so on both platforms it never crosses the shell's text pipe — binary-safe)
pub fn ssh_run_with_stdin(host: &str, cmd: &str, stdin_file: &Path) -> std::io::Result<()> {
    let f = std::fs::File::open(stdin_file)?;
    let mut child = ssh_base(host)
        .arg(cmd)
        .stdin(Stdio::from(f))
        .stderr(Stdio::inherit())
        .spawn()?;
    let status = child.wait()?;
    if !status.success() {
        return Err(Error::new(ErrorKind::Other, format!("stdin-ship to {host} failed: {cmd}")));
    }
    Ok(())
}
