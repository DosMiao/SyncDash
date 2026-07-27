//! Root directory lock (protocol modelled on FFS base/dir_lock.cpp, simplified):
//! - before apply touches anything, drop a .syncdash.lock in each root (JSON: host/pid/start time)
//! - the holder refreshes the lock file's mtime every 4s (heartbeat) — visible to the other machine even over SMB
//! - a contender that finds an existing lock watches it for 12s: heartbeat still beating → refuse to run; dead still → declared stale, take over
//! - on Drop: stop the heartbeat, delete the lock

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::foundation::names::LOCK_NAME;
const WATCH_ROUNDS: u32 = 6; // 6 × 2s = 12s observation window
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
                        vanished = true; // the other side finished cleanly
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
                crate::log_warn!("lock", "stale lock from {holder} on {} — taking over", root.display());
                let _ = std::fs::remove_file(&path);
            }
        }
        let info = LockInfo {
            host: crate::table::host_name(),
            pid: std::process::id(),
            started_ms: crate::foundation::time::now_ms(),
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
