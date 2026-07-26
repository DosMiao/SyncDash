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

/// 把本地文件经 stdin 管道送到远端路径（等效 scp，但零额外依赖、零引号问题）
pub fn ssh_send_file(host: &str, local: &Path, remote_path: &str) -> std::io::Result<()> {
    let f = std::fs::File::open(local)?;
    let mut child = ssh_base(host)
        .arg(format!("cat > {}", shq(remote_path)))
        .stdin(Stdio::from(f))
        .stderr(Stdio::inherit())
        .spawn()?;
    let status = child.wait()?;
    if !status.success() {
        return Err(Error::new(ErrorKind::Other, format!("ship to {host}:{remote_path} failed")));
    }
    Ok(())
}
