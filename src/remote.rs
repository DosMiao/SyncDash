//! ssh 传输原语（v0.6）。全部走 `ssh -o BatchMode=yes`（免密已配好，密码提示直接失败不挂起）。
//! 从 Rust 直接起进程，绕开 PowerShell 5.1 的嵌套引号地狱。
//! 注：远端命令按 POSIX shell 引号规则拼接（mac/linux 远端）；Windows 远端（默认 shell 是
//! PowerShell）暂不支持，列入 roadmap。

use std::io::{Error, ErrorKind};
use std::path::Path;
use std::process::{Command, Stdio};

fn ssh_base(host: &str) -> Command {
    let mut c = Command::new("ssh");
    c.args(["-o", "BatchMode=yes", host]);
    c
}

/// POSIX 单引号包裹（内嵌单引号转义为 '\''）
pub fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 远端 shell 方言：Windows 的 OpenSSH 默认 shell 是 PowerShell
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

/// 按远端方言引号包裹（PowerShell 单引号内嵌单引号写成 ''）
pub fn shq_for(shell: RemoteShell, s: &str) -> String {
    match shell {
        RemoteShell::Posix => shq(s),
        RemoteShell::PowerShell => format!("'{}'", s.replace('\'', "''")),
    }
}

/// 组装远端 syncdash 调用：
/// - posix：exe 裸放（让 ~ 展开），参数逐个 shq
/// - powershell：`chcp 65001 >$null; & '<exe>' 参数...`（先切 UTF-8 防中文路径被 OEM 码页搅坏）
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

/// 远端执行并捕获 stdout（stderr 直通本地终端）
pub fn ssh_capture(host: &str, cmd: &str) -> std::io::Result<Vec<u8>> {
    let out = ssh_base(host).arg(cmd).stderr(Stdio::inherit()).output()?;
    if !out.status.success() {
        return Err(Error::new(ErrorKind::Other, format!("ssh command failed on {host}: {cmd}")));
    }
    Ok(out.stdout)
}

/// 远端执行，stdout/stderr 直通。返回是否成功。
pub fn ssh_run(host: &str, cmd: &str) -> std::io::Result<bool> {
    let status = ssh_base(host).arg(cmd).status()?;
    Ok(status.success())
}

/// 远端执行命令并把本地文件接到它的 stdin（配远端 `syncdash recv` 用：
/// Rust 读原始字节，双平台都不经过 shell 的文本管道，二进制可靠）
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
