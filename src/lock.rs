//! 根目录锁（参考 FFS base/dir_lock.cpp 的协议，简化版）：
//! - apply 动手前在两侧 root 各放一个 .syncdash.lock（JSON：host/pid/开始时间）
//! - 持锁进程每 4 秒刷新锁文件 mtime（心跳）——经 SMB 也能被对面机器看到
//! - 抢锁方发现已有锁时观察 12 秒：心跳还在跳 → 拒绝执行；一动不动 → 判定遗弃，接管
//! - Drop 时停心跳、删锁

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const LOCK_NAME: &str = ".syncdash.lock";
const WATCH_ROUNDS: u32 = 6; // 6 × 2s = 12s 观察窗
const HEARTBEAT_MS: u64 = 4000;

#[derive(Serialize, Deserialize)]
struct LockInfo {
    host: String,
    pid: u32,
    started_ms: u64,
}

pub struct RootLock {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    heartbeat: Option<std::thread::JoinHandle<()>>,
}

fn mtime_of(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl RootLock {
    pub fn acquire(root: &Path) -> std::io::Result<RootLock> {
        let path = root.join(LOCK_NAME);
        if path.exists() {
            let holder = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<LockInfo>(&s).ok())
                .map(|i| format!("{} (pid {})", i.host, i.pid))
                .unwrap_or_else(|| "unknown".into());
            let m0 = mtime_of(&path);
            let mut vanished = false;
            for _ in 0..WATCH_ROUNDS {
                std::thread::sleep(std::time::Duration::from_millis(2000));
                match mtime_of(&path) {
                    None => {
                        vanished = true; // 对方正常收工
                        break;
                    }
                    Some(m) if Some(m) != m0 => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            format!("{} is being synced right now by {holder} — try again later", root.display()),
                        ));
                    }
                    _ => {}
                }
            }
            if !vanished {
                eprintln!("stale lock from {holder} on {} — taking over", root.display());
                let _ = std::fs::remove_file(&path);
            }
        }
        let info = LockInfo {
            host: crate::table::host_name(),
            pid: std::process::id(),
            started_ms: crate::table::now_ms(),
        };
        std::fs::write(&path, serde_json::to_string(&info)?)?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let path2 = path.clone();
        let heartbeat = std::thread::spawn(move || {
            let mut elapsed = 0u64;
            while !stop2.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(250));
                elapsed += 250;
                if elapsed >= HEARTBEAT_MS {
                    elapsed = 0;
                    let now = filetime::FileTime::now();
                    let _ = filetime::set_file_mtime(&path2, now);
                }
            }
        });

        Ok(RootLock { path, stop, heartbeat: Some(heartbeat) })
    }
}

impl Drop for RootLock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.heartbeat.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}
