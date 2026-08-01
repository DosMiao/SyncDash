//! What a single op does, dispatched by action.
//!
//! Copy and Update share a lane — the difference between "the target has nothing" and "the target
//! has the wrong thing" matters to the plan, not to the write. Both stage, verify if asked, and
//! land by rename.


use crate::foundation::path::join_native;
use crate::model::plan::{Action, Op, Side};
use super::delta::update_with_delta;
use super::dir::{try_delete_dir_vfs, DirOutcome};
use crate::obs::progress::PhaseProgress;
use std::sync::atomic::Ordering;
use super::platform::{exists_no_follow, read_mtime_ms, set_mode, set_mtime};
use super::preserve::preserve;
use super::schedule::Shared;

/// Read granularity for the local re-read in the delta lane, and therefore how often
/// cancel/pause is honoured mid-file. Same 8 MiB the scan lane reads in.
const READ_CHUNK: u64 = 8 * 1024 * 1024;

/// Execute a single op through the VFS pair. Cancel/pause are honored at chunk
/// boundaries via `pp.checkpoint()`; a cancel returns Interrupted, and the staged
/// write's Drop contract guarantees no debris at the destination on either backend.
pub(super) fn exec_op(sh: &Shared, op: &Op, pp: &PhaseProgress) -> std::io::Result<()> {
    use crate::fs::vfs::WriteHint;
    let (exec, other) = sh.exec_other(&op.side);
    match op.action {
        Action::Copy | Action::Update => {
            if let Some(parent) = crate::foundation::path::parent(&op.path) {
                sh.ensure_dir(&op.side, exec, parent)?;
            }
            // symlink op: create the link itself, don't copy content (a link is metadata; atomic writes don't apply)
            if let Some(target) = &op.link {
                if exec.stat(&op.path)?.is_some() {
                    preserve(sh, op, exec, "overwritten", None, pp)?;
                }
                return Ok(exec.make_symlink(&op.path, target)?);
            }

            // Plan sizes are exact for ordinary copy/update rows. Only pay for another stat when a
            // legacy/package op omitted size or mtime; the final streamed count still reconciles
            // a source that changed after compare without adding one round-trip per remote file.
            let live_meta = if op.size.is_none() || op.mtime_ms.is_none() {
                other.stat(&op.path)?
            } else {
                None
            };
            let planned_size = match op.size {
                Some(size) => size,
                None => live_meta.as_ref().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("copy source disappeared after compare: {}", op.path),
                    )
                })?.size,
            };
            pp.revise_total_bytes(op.size.unwrap_or(0), planned_size);

            // Delta stays a both-local affair — it reads old and new into memory and
            // patches with write_at. Preflight already disabled it otherwise; this gate
            // is the belt to that braces.
            let exec_local = sh.local_of(&op.side);
            let other_local = sh.local_of(match op.side {
                Side::Target => &Side::Source,
                Side::Source => &Side::Target,
            });
            if sh.opt.delta && op.action == Action::Update {
                if let (Some(eroot), Some(oroot)) = (exec_local, other_local) {
                    let dst = join_native(eroot, &op.path);
                    let src = join_native(oroot, &op.path);
                    if exists_no_follow(&dst) {
                        let mut staged = crate::fs::staged::Staged::create(&dst)?;
                        if let Some((written, total, h)) = update_with_delta(&src, &dst, &mut staged)? {
                            pp.revise_total_bytes(planned_size, total);
                            sh.delta_saved.fetch_add(total.saturating_sub(written), Ordering::Relaxed);
                            pp.add_bytes(total, &op.path);
                            staged.seal(sh.opt.fsync)?;
                            if sh.opt.verify {
                                let mut hasher = blake3::Hasher::new();
                                let mut f = std::fs::File::open(staged.path())?;
                                let mut buf = vec![0u8; total.clamp(1, READ_CHUNK) as usize];
                                loop {
                                    pp.checkpoint()?;
                                    let n = std::io::Read::read(&mut f, &mut buf)?;
                                    if n == 0 {
                                        break;
                                    }
                                    hasher.update(&buf[..n]);
                                }
                                let got = hasher.finalize().to_hex().to_string();
                                if got != h {
                                    return Err(std::io::Error::new(
                                        std::io::ErrorKind::InvalidData,
                                        format!("write verify failed: staged readback {got} != copy stream {h}"),
                                    ));
                                }
                            }
                            let intended = match op.mtime_ms {
                                Some(mt) => {
                                    set_mtime(staged.path(), mt);
                                    Some(mt)
                                }
                                None => read_mtime_ms(&src).inspect(|mt| set_mtime(staged.path(), *mt)),
                            };
                            if let Some(m) = op.mode {
                                set_mode(staged.path(), m)?;
                            }
                            if exists_no_follow(&dst) {
                                preserve(sh, op, exec, "overwritten", Some(&src), pp)?;
                            }
                            staged.commit()?;
                            if let Some(want) = intended {
                                if let Some(got) = read_mtime_ms(&dst) {
                                    if got != want {
                                        sh.mtime_fixes.lock().unwrap().push((op.side == Side::Source, op.path.clone(), got, want));
                                    }
                                }
                            }
                            return Ok(());
                        }
                        // Not delta-eligible (size caps): the staged handle drops clean, generic lane below
                    }
                }
            }

            // The generic lane: stream from `other`, stage on `exec`, rename into place.
            // The expected value for post-copy verification = **the full blake3 of this
            // copy stream** — not op.hash, which may be only a sampled `~` digest.
            let intended = match op.mtime_ms {
                Some(mt) => Some(mt),
                None => live_meta.as_ref().map(|m| m.mtime_ms),
            };
            let hint = WriteHint { size_hint: Some(planned_size), mtime_ms: intended, mode: op.mode };
            let mut w = exec.open_write(&op.path, &hint)?;
            let mut src_stream = other.open_read(&op.path)?;
            let block = src_stream.block_size().max(w.block_size()).clamp(64 * 1024, 8 * 1024 * 1024);
            let mut buf = vec![0u8; block];
            let mut hasher = if sh.opt.verify { Some(blake3::Hasher::new()) } else { None };
            let mut copied = 0u64;
            loop {
                pp.checkpoint()?;
                let n = std::io::Read::read(&mut src_stream, &mut buf)?;
                if n == 0 {
                    break;
                }
                w.write(&buf[..n])?;
                if let Some(h) = hasher.as_mut() {
                    h.update(&buf[..n]);
                }
                copied += n as u64;
                pp.add_bytes(n as u64, &op.path);
            }
            pp.revise_total_bytes(planned_size, copied);
            w.seal(sh.opt.fsync)?;

            // Length reconciliation (FFS's finalize check — it has caught corrupt
            // transfers in the wild): what the backend holds must equal what we sent
            let staged_len = w.staged_len()?;
            if staged_len != copied {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("staged file holds {staged_len} bytes but the copy stream carried {copied}"),
                ));
            }

            // Verification runs on the staged file: readback vs the copy stream — a failure never becomes the final file
            if let Some(h) = hasher {
                let expect = h.finalize().to_hex().to_string();
                let mut rs = w.open_staged_read()?;
                let mut hh = blake3::Hasher::new();
                let mut b2 = vec![0u8; block];
                loop {
                    pp.checkpoint()?;
                    let n = std::io::Read::read(&mut rs, &mut b2)?;
                    if n == 0 {
                        break;
                    }
                    hh.update(&b2[..n]);
                }
                let got = hh.finalize().to_hex().to_string();
                if got != expect {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("write verify failed: staged readback {got} != copy stream {expect}"),
                    ));
                }
            }

            // Archive the old file at the last moment before commit, so the window is a single rename
            if exec.stat(&op.path)?.is_some() {
                let newer = other_local.map(|r| join_native(r, &op.path));
                preserve(sh, op, exec, "overwritten", newer.as_deref(), pp)?;
            }
            let report = w.commit()?;

            // mtime bookkeeping (P1-4): the backend reports what actually landed
            if let Some(want) = intended {
                if let Some(got) = report.mtime_ondisk_ms {
                    if got != want {
                        sh.mtime_fixes.lock().unwrap().push((op.side == Side::Source, op.path.clone(), got, want));
                    }
                }
                if let Some(e) = report.mtime_error {
                    // Not a copy failure (FFS's errorModTime lesson) — but never silent either
                    pp.error(&op.path, "set_mtime", if op.side == Side::Target { "target" } else { "source" },
                        &format!("mtime could not be set ({e}); comparison will lean on size/content for this file"));
                }
            }
            if let Some(e) = report.mode_error {
                pp.error(&op.path, "chmod", if op.side == Side::Target { "target" } else { "source" },
                    &format!("permissions could not be set ({e})"));
            }
            Ok(())
        }
        Action::Chmod => {
            let m = op.mode.ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "chmod op without mode")
            })?;
            // A plan carrying modes only arises unix↔unix; an executing side without
            // unix modes (Windows) skips, exactly as the path lane always did
            if exec.caps().unix_mode != crate::fs::vfs::Support::Yes {
                return Ok(());
            }
            Ok(exec.set_mode(&op.path, m)?)
        }
        Action::Move => {
            let from = op.from.as_deref().unwrap_or_default();
            if exec.stat(&op.path)?.is_some() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "move destination appeared after compare; refusing to overwrite it: {}",
                        op.path
                    ),
                ));
            }
            if let Some(parent) = crate::foundation::path::parent(&op.path) {
                sh.ensure_dir(&op.side, exec, parent)?;
            }
            match exec.rename(from, &op.path) {
                Ok(_) => Ok(()),
                Err(_) => {
                    if exec.stat(&op.path)?.is_some() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            format!(
                                "move destination appeared during execution; refusing to overwrite it: {}",
                                op.path
                            ),
                        ));
                    }
                    // Cross-volume fallback: copy within the same root, still atomic.
                    let moving = exec.stat(from)?.ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::NotFound, format!("move source disappeared: {from}"))
                    })?;
                    pp.add_total_bytes(moving.size);
                    let hint = WriteHint { size_hint: Some(moving.size), mtime_ms: Some(moving.mtime_ms), mode: None };
                    let mut w = exec.open_write(&op.path, &hint)?;
                    let mut rs = exec.open_read(from)?;
                    let mut buf = vec![0u8; rs.block_size().clamp(64 * 1024, 8 * 1024 * 1024)];
                    let mut copied = 0u64;
                    loop {
                        pp.checkpoint()?;
                        let n = std::io::Read::read(&mut rs, &mut buf)?;
                        if n == 0 {
                            break;
                        }
                        w.write(&buf[..n])?;
                        copied += n as u64;
                        pp.add_bytes(n as u64, &op.path);
                    }
                    pp.revise_total_bytes(moving.size, copied);
                    w.seal(sh.opt.fsync)?;
                    if exec.stat(&op.path)?.is_some() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            format!(
                                "move destination appeared during copy; refusing to overwrite it: {}",
                                op.path
                            ),
                        ));
                    }
                    let _ = w.commit()?;
                    Ok(exec.remove_file(from)?)
                }
            }
        }
        Action::Delete => {
            // stat is lstat: a broken symlink still reports present, so it still gets preserved
            if exec.stat(&op.path)?.is_some() {
                preserve(sh, op, exec, "deleted", None, pp)?;
            }
            Ok(())
        }
        Action::DeleteDir => {
            // P0-4: report by classification, no longer swallowed silently
            match try_delete_dir_vfs(exec, &op.path, sh.opt.filter.as_ref()) {
                DirOutcome::Removed | DirOutcome::Absent => Ok(()),
                DirOutcome::NotEmpty { sample } => Err(std::io::Error::new(
                    std::io::ErrorKind::DirectoryNotEmpty,
                    format!(
                        "directory not empty, kept: {} (protected by filters or unknown to the plan). \
                         Add them to `deletable` in the job to have them removed with the directory.",
                        sample.join(", ")
                    ),
                )),
                DirOutcome::Failed(e) => Err(e),
            }
        }
        Action::Conflict | Action::Note => Ok(()),
    }
}
