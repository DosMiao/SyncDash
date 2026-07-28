//! Root directory lock (protocol modelled on FFS base/dir_lock.cpp, simplified):
//! - before apply touches anything, drop a .syncdash.lock in each root (JSON: host/pid/start time)
//! - the holder refreshes the lock file's mtime every 4s (heartbeat) — visible to the other machine even over SMB
//! - a contender that finds an existing lock watches it for 12s: heartbeat still beating → refuse to run; dead still → declared stale, take over
//! - on Drop: stop the heartbeat, delete the lock
//!
//! v0.10: the lock speaks `Vfs`, so an sftp root gets the same mutual exclusion —
//! its heartbeat rides set_mtime (SFTP's setstat), same protocol, same observers.
//! A backend without settable mtimes (FTP without MFMT) will need the content-beat
//! variant; that lands together with the FTP write lane, not before.
//!
//! Layering: this L0 module reaches up to L1 `obs::logging` through `log_warn!`. A stale-lock
//! takeover silently discards another machine's claim on the root — the one event here that must
//! reach the operator's log, and there is no `RunCtx` this deep. See `lib.rs` for the edge.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::foundation::names::LOCK_NAME;
use crate::fs::vfs::{Vfs, WriteHint};
const WATCH_ROUNDS: u32 = 6; // 6 × 2s = 12s observation window
const HEARTBEAT_MS: u64 = 4000;

#[derive(Serialize, Deserialize)]
struct LockInfo {
    host: String,
    pid: u32,
    started_ms: u64,
}

pub struct RootLock {
    vfs: Arc<dyn Vfs>,
    stop: Arc<AtomicBool>,
    heartbeat: Option<std::thread::JoinHandle<()>>,
}

fn read_small(vfs: &Arc<dyn Vfs>, rel: &str) -> Option<String> {
    use std::io::Read;
    let mut s = String::new();
    vfs.open_read(rel).ok()?.read_to_string(&mut s).ok()?;
    Some(s)
}

/// The lock's liveness signal: its mtime as the backend reports it.
/// None = the file is gone (clean finish on the other side).
fn probe(vfs: &Arc<dyn Vfs>, rel: &str) -> std::io::Result<Option<i64>> {
    Ok(vfs.stat(rel)?.map(|m| m.mtime_ms))
}

fn write_lock(vfs: &Arc<dyn Vfs>, rel: &str, info: &LockInfo) -> std::io::Result<()> {
    let body = serde_json::to_string(info)?;
    let mut w = vfs.open_write(rel, &WriteHint::default())?;
    w.write(body.as_bytes())?;
    w.seal(false)?;
    w.commit()?;
    Ok(())
}

impl RootLock {
    pub fn acquire_vfs(vfs: &Arc<dyn Vfs>) -> std::io::Result<RootLock> {
        let rel = LOCK_NAME;
        let disp = vfs.display();
        if let Some(m0) = probe(vfs, rel)? {
            let holder = read_small(vfs, rel)
                .and_then(|s| serde_json::from_str::<LockInfo>(&s).ok())
                .map(|i| format!("{} (pid {})", i.host, i.pid))
                .unwrap_or_else(|| "unknown".into());
            let mut vanished = false;
            for _ in 0..WATCH_ROUNDS {
                std::thread::sleep(std::time::Duration::from_millis(2000));
                // A probe failure here is uncertainty, and uncertainty never takes a lock over
                match probe(vfs, rel)? {
                    None => {
                        vanished = true; // the other side finished cleanly
                        break;
                    }
                    Some(m) if m != m0 => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            format!("{disp} is being synced right now by {holder} — try again later"),
                        ));
                    }
                    _ => {}
                }
            }
            if !vanished {
                crate::log_warn!("lock", "stale lock from {holder} on {disp} — taking over");
                match vfs.remove_file(rel) {
                    Ok(()) => {}
                    Err(e) if e.kind == crate::fs::vfs::error::VfsErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        let info = LockInfo {
            host: crate::model::table::host_name(),
            pid: std::process::id(),
            started_ms: crate::foundation::time::now_ms(),
        };
        write_lock(vfs, rel, &info)?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let vfs2 = vfs.clone();
        let heartbeat = std::thread::spawn(move || {
            let mut elapsed = 0u64;
            while !stop2.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(250));
                elapsed += 250;
                if elapsed >= HEARTBEAT_MS {
                    elapsed = 0;
                    // Best-effort, like the old touch: a beat that fails to land shows
                    // up as a stale lock after 12s, which is the protocol working
                    let _ = vfs2.set_mtime(LOCK_NAME, crate::foundation::time::now_ms() as i64);
                }
            }
        });

        Ok(RootLock { vfs: vfs.clone(), stop, heartbeat: Some(heartbeat) })
    }
}

impl Drop for RootLock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.heartbeat.take() {
            let _ = h.join();
        }
        let _ = self.vfs.remove_file(LOCK_NAME);
    }
}
