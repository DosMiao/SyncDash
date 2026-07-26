//! apply：执行计划（要求 4 的本地/挂载模式）。
//! - 默认 DRY-RUN，--apply 才动手（GitDash 同款哲学：预览默认、绝不悄悄动手）
//! - 删除与被覆盖的文件先挪进本机回收目录（可找回），不做原地销毁
//! - copy 后把 mtime 设成源表里的值 —— 下次比对的相等判定依赖它
//! 远端打包模式（tar/zip + 清单 + 双 hash + 对端校验执行）在 v0.4，见 README。

use crate::compare::{Action, Op, Side};
use filetime::FileTime;
use std::path::{Path, PathBuf};

pub struct ApplyOptions {
    pub dry_run: bool,
    pub trash: Option<PathBuf>,
    pub verbose: bool,
    /// paranoid 严谨级：复制/更新后重读目标文件校验 blake3（FFS "verify copied files" 同款）
    pub verify: bool,
}

fn default_trash() -> PathBuf {
    let base = if let Ok(l) = std::env::var("LOCALAPPDATA") {
        PathBuf::from(l).join("syncdash")
    } else if let Ok(h) = std::env::var("HOME") {
        PathBuf::from(h).join(".cache").join("syncdash")
    } else {
        PathBuf::from(".syncdash")
    };
    base.join("trash").join(crate::table::now_ms().to_string())
}

fn to_native(rel: &str) -> String {
    if cfg!(windows) { rel.replace('/', "\\") } else { rel.to_string() }
}

fn move_to_trash(file: &Path, rel: &str, trash: &Path) -> std::io::Result<()> {
    let dest = trash.join(to_native(rel));
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    match std::fs::rename(file, &dest) {
        Ok(_) => Ok(()),
        Err(_) => {
            // 跨卷：退化为 copy+delete
            std::fs::copy(file, &dest)?;
            std::fs::remove_file(file)
        }
    }
}

fn set_mtime(path: &Path, mtime_ms: i64) {
    let ft = FileTime::from_unix_time(mtime_ms / 1000, ((mtime_ms % 1000) * 1_000_000) as u32);
    let _ = filetime::set_file_mtime(path, ft);
}

pub fn apply(ops: &[Op], source_root: &Path, target_root: &Path, opt: &ApplyOptions) -> (u64, u64, u64) {
    // FFS dir_lock 思路：动手前锁两侧 root（带心跳），防两台机器同时 apply 同一目录
    let _lock_guard: Option<(crate::lock::RootLock, crate::lock::RootLock)> = if opt.dry_run {
        None
    } else {
        let ls = match crate::lock::RootLock::acquire(source_root) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("cannot lock source root: {e}");
                return (0, ops.len() as u64, 1);
            }
        };
        let lt = match crate::lock::RootLock::acquire(target_root) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("cannot lock target root: {e}");
                return (0, ops.len() as u64, 1);
            }
        };
        Some((ls, lt))
    };

    let trash = opt.trash.clone().unwrap_or_else(default_trash);
    let mut done = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;

    for op in ops {
        let (exec_root, other_root) = match op.side {
            Side::Target => (target_root, source_root),
            Side::Source => (source_root, target_root),
        };
        let label = format!("[{}] {:?} {}", if op.side == Side::Target { "target" } else { "source" }, op.action, op.path);
        match op.action {
            Action::Conflict => {
                println!("CONFLICT  {} ({})", op.path, op.reason);
                skipped += 1;
                continue;
            }
            Action::Note => {
                println!("NOTE      {} ({} from={})", op.path, op.reason, op.from.clone().unwrap_or_default());
                skipped += 1;
                continue;
            }
            _ => {}
        }
        if opt.dry_run {
            if opt.verbose {
                println!("DRY  {label}  ({})", op.reason);
            }
            skipped += 1;
            continue;
        }
        let res: std::io::Result<()> = (|| {
            let dst = exec_root.join(to_native(&op.path));
            match op.action {
                Action::Copy | Action::Update => {
                    let src = other_root.join(to_native(&op.path));
                    if let Some(p) = dst.parent() {
                        std::fs::create_dir_all(p)?;
                    }
                    if op.action == Action::Update && dst.exists() {
                        move_to_trash(&dst, &op.path, &trash)?;
                    }
                    std::fs::copy(&src, &dst)?;
                    if opt.verify {
                        if let Some(expect) = &op.hash {
                            let mut hasher = blake3::Hasher::new();
                            hasher.update_mmap_rayon(&dst)?;
                            let got = hasher.finalize().to_hex().to_string();
                            if &got != expect {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    format!("verify failed after copy: expected {expect}, got {got}"),
                                ));
                            }
                        }
                    }
                    if let Some(mt) = op.mtime_ms {
                        set_mtime(&dst, mt);
                    } else if let Ok(md) = std::fs::metadata(&src) {
                        if let Ok(t) = md.modified() {
                            let _ = filetime::set_file_mtime(&dst, FileTime::from_system_time(t));
                        }
                    }
                    Ok(())
                }
                Action::Move => {
                    let from = exec_root.join(to_native(op.from.as_deref().unwrap_or_default()));
                    if let Some(p) = dst.parent() {
                        std::fs::create_dir_all(p)?;
                    }
                    match std::fs::rename(&from, &dst) {
                        Ok(_) => Ok(()),
                        Err(_) => {
                            std::fs::copy(&from, &dst)?;
                            std::fs::remove_file(&from)
                        }
                    }
                }
                Action::Delete => {
                    if dst.exists() {
                        move_to_trash(&dst, &op.path, &trash)?;
                    }
                    Ok(())
                }
                Action::DeleteDir => {
                    match std::fs::remove_dir(&dst) {
                        Ok(_) => Ok(()),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                        Err(_) => Ok(()), // 非空（里面还有被排除的文件等）：保留，不视为错误
                    }
                }
                Action::Conflict | Action::Note => Ok(()),
            }
        })();
        match res {
            Ok(_) => {
                done += 1;
                if opt.verbose {
                    println!("OK   {label}");
                }
            }
            Err(e) => {
                errors += 1;
                eprintln!("ERR  {label}: {e}");
            }
        }
    }

    if !opt.dry_run && done > 0 {
        println!("trash (deleted/overwritten files kept at): {}", trash.display());
    }
    (done, skipped, errors)
}
