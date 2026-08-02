//! What a single op does, dispatched by action.
//!
//! Copy and Update share a lane — the difference between "the target has nothing" and "the target
//! has the wrong thing" matters to the plan, not to the write. Both stage, verify if asked, and
//! land by rename.

use super::delta::stage_delta_update;
use super::dir::{try_delete_dir_vfs, DirOutcome};
use super::preserve::preserve;
use super::schedule::Shared;
use crate::foundation::names::TEMP_PREFIX;
use crate::foundation::path::RootRelativePath;
use crate::fs::vfs::error::VfsErrorKind;
use crate::fs::vfs::{VMeta, Vfs};
use crate::model::plan::{Action, Op, Side};
use crate::obs::progress::PhaseProgress;
use std::sync::atomic::{AtomicU64, Ordering};

/// Read granularity for the local re-read in the delta lane, and therefore how often
/// cancel/pause is honoured mid-file. Same 8 MiB the scan lane reads in.
const READ_CHUNK: u64 = 8 * 1024 * 1024;
const SAMPLE_CHUNK: u64 = 256 * 1024;
static NEXT_MOVE_HOLD_ID: AtomicU64 = AtomicU64::new(1);

fn next_move_hold(from: &str) -> String {
    let parent = crate::foundation::path::parent(from).unwrap_or("");
    let id = NEXT_MOVE_HOLD_ID.fetch_add(1, Ordering::Relaxed);
    // A valid near-NAME_MAX source must still have a valid hold name. Keep the original path only
    // in the error/reporting context and use a bounded digest in the on-disk artifact name.
    let digest = blake3::hash(from.as_bytes()).to_hex();
    let digest = &digest.as_str()[..16];
    let name = format!("{TEMP_PREFIX}move.{digest}.{}.{id}", std::process::id());
    if parent.is_empty() {
        name
    } else {
        format!("{parent}/{name}")
    }
}

fn sync_local_parent(exec: &dyn Vfs, rel: &str, fsync: bool) -> std::io::Result<()> {
    if !fsync {
        return Ok(());
    }
    if let Some(root) = exec.local_root() {
        root.sync_parent(&relative_path(rel)?)?;
    }
    Ok(())
}

fn sync_local_rename_parents(
    exec: &dyn Vfs,
    from: &str,
    to: &str,
    fsync: bool,
) -> std::io::Result<()> {
    if !fsync {
        return Ok(());
    }
    if let Some(root) = exec.local_root() {
        root.sync_parent(&relative_path(to)?)?;
        if crate::foundation::path::parent(from) != crate::foundation::path::parent(to) {
            root.sync_parent(&relative_path(from)?)?;
        }
    }
    Ok(())
}

fn relative_path(relative: &str) -> std::io::Result<RootRelativePath> {
    RootRelativePath::try_from(relative)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

fn apply_commit_report(
    sh: &Shared<'_>,
    op: &Op,
    intended_mtime_ms: Option<i64>,
    report: crate::fs::vfs::CommitReport,
    pp: &PhaseProgress<'_>,
) -> std::io::Result<()> {
    let side = if op.side == Side::Target {
        "target"
    } else {
        "source"
    };
    if let Some(want) = intended_mtime_ms {
        if let Some(got) = report.mtime_ondisk_ms {
            if got != want {
                sh.mtime_fixes.lock().unwrap().push((
                    op.side == Side::Source,
                    op.path.clone(),
                    got,
                    want,
                ));
            }
        }
        if let Some(error) = report.mtime_error {
            pp.error(
                &op.path,
                "set_mtime",
                side,
                &format!(
                    "mtime could not be set ({error}); comparison will lean on size/content for this file"
                ),
            );
        }
    }
    if let Some(error) = report.mode_error {
        return Err(std::io::Error::other(format!(
            "file content was published, but requested permissions could not be set ({error})"
        )));
    }
    Ok(())
}

fn claim_move_source(
    sh: &Shared<'_>,
    exec: &dyn Vfs,
    from: &str,
    fsync: bool,
) -> std::io::Result<String> {
    // Retrying AlreadyExists is safe because the source has not moved in that case. Every other
    // failure is final and occurs before destination work.
    for _ in 0..1024 {
        let hold = next_move_hold(from);
        sh.check_before_mutation()?;
        match exec.rename_noreplace(from, &hold) {
            Ok(()) => {
                if let Err(error) = sync_local_rename_parents(exec, from, &hold, fsync) {
                    let primary = std::io::Error::new(
                        error.kind(),
                        format!(
                            "move source was claimed at '{hold}', but its directory could not be synced ({error})"
                        ),
                    );
                    return Err(rollback_claim(exec, &hold, from, primary, fsync));
                }
                return Ok(hold);
            }
            Err(error) if error.kind == VfsErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("could not reserve a unique move hold for {from}"),
    ))
}

fn rollback_claim(
    exec: &dyn Vfs,
    hold: &str,
    from: &str,
    primary: std::io::Error,
    fsync: bool,
) -> std::io::Error {
    match exec.rename_noreplace(hold, from) {
        Ok(()) => match sync_local_rename_parents(exec, hold, from, fsync) {
            Ok(()) => primary,
            Err(sync_error) => std::io::Error::new(
                primary.kind(),
                format!(
                    "{primary}; source was restored to '{from}', but rollback directory sync failed ({sync_error})"
                ),
            ),
        },
        Err(rollback) => {
            let detail = if rollback.kind == VfsErrorKind::AlreadyExists {
                format!(
                    "{primary}; the source name reappeared, so the claimed original was retained at recoverable path '{hold}'"
                )
            } else {
                format!(
                    "{primary}; rollback to '{from}' failed ({rollback}); claimed data may be recovered at '{hold}'"
                )
            };
            std::io::Error::new(primary.kind(), detail)
        }
    }
}

fn stream_hash(
    sh: &Shared<'_>,
    stream: &mut dyn crate::fs::vfs::ReadStream,
    pp: &PhaseProgress<'_>,
) -> std::io::Result<(u64, String)> {
    let mut hasher = blake3::Hasher::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; stream.block_size().clamp(64 * 1024, READ_CHUNK as usize)];
    loop {
        sh.checkpoint(pp)?;
        let n = std::io::Read::read(stream, &mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((total, hasher.finalize().to_hex().to_string()))
}

fn move_evidence_hash(
    sh: &Shared<'_>,
    exec: &dyn Vfs,
    rel: &str,
    size: u64,
    sampled: bool,
    pp: &PhaseProgress<'_>,
) -> std::io::Result<String> {
    if sampled {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&size.to_le_bytes());
        for off in [0u64, size / 2, size.saturating_sub(SAMPLE_CHUNK)] {
            sh.checkpoint(pp)?;
            let bytes = exec.read_range(rel, off, SAMPLE_CHUNK as u32)?;
            let expected = size.saturating_sub(off).min(SAMPLE_CHUNK) as usize;
            if bytes.len() != expected {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!(
                        "move source '{rel}' changed while sampling: expected {expected} bytes at offset {off}, read {}",
                        bytes.len()
                    ),
                ));
            }
            hasher.update(&bytes);
        }
        Ok(format!("~{}", hasher.finalize().to_hex()))
    } else {
        let mut stream = exec.open_read(rel)?;
        let (read, hash) = stream_hash(sh, &mut *stream, pp)?;
        if read != size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("move source '{rel}' changed size: expected {size}, read {read}"),
            ));
        }
        Ok(hash)
    }
}

fn verify_move_evidence(
    sh: &Shared<'_>,
    exec: &dyn Vfs,
    hold: &str,
    op: &Op,
    pp: &PhaseProgress<'_>,
) -> std::io::Result<VMeta> {
    let expected_size = op.size.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("move operation has no source-size evidence: {}", op.path),
        )
    })?;
    if op.hash.is_none() && op.mtime_ms.is_none() && op.link.is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "move operation has neither hash nor mtime source evidence: {}",
                op.path
            ),
        ));
    }
    let before = exec.stat(hold)?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("claimed move source disappeared: {hold}"),
        )
    })?;
    let expected_kind = if op.link.is_some() {
        crate::fs::vfs::VfsEntryKind::Symlink
    } else {
        crate::fs::vfs::VfsEntryKind::File
    };
    if before.kind != expected_kind {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "claimed move source changed kind: expected {expected_kind:?}, found {:?}: {hold}",
                before.kind
            ),
        ));
    }
    if expected_kind == crate::fs::vfs::VfsEntryKind::File && before.size != expected_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "move source changed size after compare: expected {expected_size}, found {}",
                before.size
            ),
        ));
    }
    if expected_kind == crate::fs::vfs::VfsEntryKind::File {
        if let Some(expected_mode) = op.mode {
            if before.mode.is_some_and(|mode| mode != expected_mode) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("move source mode changed after compare: {hold}"),
                ));
            }
        }
    }
    if let Some(expected_target) = op.link.as_deref() {
        let target = exec.read_link(hold)?;
        if target != expected_target {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("move source symlink target changed after compare: {hold}"),
            ));
        }
    } else if let Some(expected_hash) = &op.hash {
        let got = move_evidence_hash(
            sh,
            exec,
            hold,
            expected_size,
            expected_hash.starts_with('~'),
            pp,
        )?;
        if &got != expected_hash {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("move source content changed after compare: {hold}"),
            ));
        }
    } else if op.mtime_ms != Some(before.mtime_ms) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("move source mtime changed after compare: {hold}"),
        ));
    }
    let after = exec.stat(hold)?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("claimed move source disappeared during verification: {hold}"),
        )
    })?;
    let symlink_target_changed = if let Some(expected_target) = op.link.as_deref() {
        exec.read_link(hold)? != expected_target
    } else {
        false
    };
    if after.kind != before.kind
        || after.size != before.size
        || after.mtime_ms != before.mtime_ms
        || after.mode != before.mode
        || symlink_target_changed
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("move source changed during verification: {hold}"),
        ));
    }
    Ok(after)
}

fn copy_claimed_move(
    sh: &Shared<'_>,
    exec: &dyn Vfs,
    op: &Op,
    hold: &str,
    moving: &VMeta,
    pp: &PhaseProgress<'_>,
) -> std::io::Result<()> {
    use crate::fs::vfs::WriteHint;

    pp.add_total_bytes(moving.size);
    let hint = WriteHint {
        size_hint: Some(moving.size),
        mtime_ms: op.mtime_ms.or(Some(moving.mtime_ms)),
        mode: op.mode.or(moving.mode),
    };
    sh.check_before_mutation()?;
    let mut writer = exec.open_write(&op.path, &hint)?;
    let mut source = exec.open_read(hold)?;
    let mut copy_hash = blake3::Hasher::new();
    let mut copied = 0u64;
    let mut buf = vec![0u8; source.block_size().clamp(64 * 1024, READ_CHUNK as usize)];
    loop {
        sh.checkpoint(pp)?;
        let n = std::io::Read::read(&mut source, &mut buf)?;
        if n == 0 {
            break;
        }
        writer.write(&buf[..n])?;
        copy_hash.update(&buf[..n]);
        copied += n as u64;
        pp.add_bytes(n as u64, &op.path);
    }
    if copied != moving.size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "move source changed during copy: expected {} bytes, copied {copied}",
                moving.size
            ),
        ));
    }
    let staged_len = writer.staged_len()?;
    if staged_len != copied {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("staged move holds {staged_len} bytes but the source stream carried {copied}"),
        ));
    }
    let copy_hash = copy_hash.finalize().to_hex().to_string();
    if op
        .hash
        .as_ref()
        .is_some_and(|expected| !expected.starts_with('~') && expected != &copy_hash)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("move source content changed during copy: {hold}"),
        ));
    }

    sh.checkpoint(pp)?;
    writer.seal(sh.opt.fsync)?;
    let mut staged = writer.open_staged_read()?;
    let (read_back_len, read_back_hash) = stream_hash(sh, &mut *staged, pp)?;
    if read_back_len != copied || read_back_hash != copy_hash {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "staged move verification failed: source {copied}/{copy_hash}, staged {read_back_len}/{read_back_hash}"
            ),
        ));
    }
    drop(staged);

    // Detect a source that changed through an already-open external handle while it was copied.
    verify_move_evidence(sh, exec, hold, op, pp)?;
    sh.check_before_mutation()?;
    let report = writer.commit_noreplace()?;
    apply_commit_report(sh, op, hint.mtime_ms, report, pp)
}

/// Execute a single op through the VFS pair. Cancel/pause are honored at chunk
/// boundaries via `Shared::checkpoint`; a cancel or lost lease returns Interrupted, and the staged
/// write's Drop contract guarantees no debris at the destination on either backend.
pub(super) fn exec_op(sh: &Shared, op: &Op, pp: &PhaseProgress) -> std::io::Result<()> {
    use crate::fs::vfs::WriteHint;
    sh.checkpoint(pp)?;
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
                sh.check_before_mutation()?;
                return Ok(exec.make_symlink(&op.path, target)?);
            }

            // Plan sizes are exact for ordinary copy/update rows. Only pay for another stat when a
            // legacy/package op omitted size or mtime; the final streamed count still reconciles
            // a source that changed after Compare without adding one round-trip per network file.
            let live_meta = if op.size.is_none() || op.mtime_ms.is_none() {
                other.stat(&op.path)?
            } else {
                None
            };
            let planned_size = match op.size {
                Some(size) => size,
                None => {
                    live_meta
                        .as_ref()
                        .ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                format!("copy source disappeared after compare: {}", op.path),
                            )
                        })?
                        .size
                }
            };
            pp.revise_total_bytes(op.size.unwrap_or(0), planned_size);

            let exec_local = sh.local_root_of(&op.side);
            let other_local = sh.local_root_of(match op.side {
                Side::Target => &Side::Source,
                Side::Source => &Side::Target,
            });
            if sh.opt.delta && op.action == Action::Update {
                if let (Some(eroot), Some(oroot)) = (exec_local, other_local) {
                    let relative = relative_path(&op.path)?;
                    let destination_exists = match eroot.metadata_path(&relative) {
                        Ok(_) => true,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                        Err(error) => return Err(error),
                    };
                    if destination_exists {
                        sh.check_before_mutation()?;
                        let mut staged = eroot.create_staged(&relative)?;
                        if let Some(delta) =
                            stage_delta_update(oroot, eroot, &relative, &mut staged, || {
                                sh.checkpoint(pp)
                            })?
                        {
                            pp.revise_total_bytes(planned_size, delta.output_size);
                            if op.size.is_some() && delta.output_size != planned_size {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    format!(
                                        "copy source changed size after compare: expected {planned_size}, read {}",
                                        delta.output_size
                                    ),
                                ));
                            }
                            if op.hash.as_ref().is_some_and(|expected| {
                                !expected.starts_with('~') && expected != &delta.output_hash
                            }) {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    format!(
                                        "copy source content changed after compare: {}",
                                        op.path
                                    ),
                                ));
                            }
                            sh.delta_saved.fetch_add(
                                delta.output_size.saturating_sub(delta.written_bytes),
                                Ordering::Relaxed,
                            );
                            pp.add_bytes(delta.output_size, &op.path);
                            sh.checkpoint(pp)?;
                            staged.seal(sh.opt.fsync)?;
                            if sh.opt.verify {
                                let mut hasher = blake3::Hasher::new();
                                let mut file = staged.try_clone_file()?;
                                let mut buf =
                                    vec![0u8; delta.output_size.clamp(1, READ_CHUNK) as usize];
                                loop {
                                    sh.checkpoint(pp)?;
                                    let n = std::io::Read::read(&mut file, &mut buf)?;
                                    if n == 0 {
                                        break;
                                    }
                                    hasher.update(&buf[..n]);
                                }
                                let got = hasher.finalize().to_hex().to_string();
                                if got != delta.output_hash {
                                    return Err(std::io::Error::new(
                                        std::io::ErrorKind::InvalidData,
                                        format!(
                                            "write verify failed: staged readback {got} != copy stream {}",
                                            delta.output_hash
                                        ),
                                    ));
                                }
                            }
                            sh.checkpoint(pp)?;
                            let intended = op
                                .mtime_ms
                                .or_else(|| live_meta.as_ref().map(|metadata| metadata.mtime_ms));
                            let mtime_error =
                                intended.and_then(|mtime| staged.set_mtime(mtime).err());
                            if let Some(m) = op.mode {
                                #[cfg(unix)]
                                staged.set_mode(m)?;
                            }
                            if sh.opt.fsync {
                                staged.sync_file()?;
                            }
                            match eroot.metadata_path(&relative) {
                                Ok(_) => {
                                    preserve(
                                        sh,
                                        op,
                                        exec,
                                        "overwritten",
                                        Some((oroot, &relative)),
                                        pp,
                                    )?;
                                }
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                                Err(error) => return Err(error),
                            }
                            sh.check_before_mutation()?;
                            staged.commit_noreplace()?;
                            if let Some(want) = intended {
                                let metadata = eroot.metadata_path(&relative)?;
                                let got = crate::foundation::time::systime_ms(
                                    metadata.modified()?.into_std(),
                                );
                                if got != want {
                                    sh.mtime_fixes.lock().unwrap().push((
                                        op.side == Side::Source,
                                        op.path.clone(),
                                        got,
                                        want,
                                    ));
                                }
                                if let Some(error) = mtime_error {
                                    pp.error(
                                        &op.path,
                                        "set_mtime",
                                        if op.side == Side::Target {
                                            "target"
                                        } else {
                                            "source"
                                        },
                                        &format!(
                                            "mtime could not be set ({error}); comparison will lean on size/content for this file"
                                        ),
                                    );
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
            let hint = WriteHint {
                size_hint: Some(planned_size),
                mtime_ms: intended,
                mode: op.mode,
            };
            sh.check_before_mutation()?;
            let mut w = exec.open_write(&op.path, &hint)?;
            let mut src_stream = other.open_read(&op.path)?;
            let block = src_stream
                .block_size()
                .max(w.block_size())
                .clamp(64 * 1024, 8 * 1024 * 1024);
            let mut buf = vec![0u8; block];
            let expected_full_hash = op
                .hash
                .as_deref()
                .filter(|expected| !expected.starts_with('~'));
            let mut hasher = if sh.opt.verify || expected_full_hash.is_some() {
                Some(blake3::Hasher::new())
            } else {
                None
            };
            let mut copied = 0u64;
            loop {
                sh.checkpoint(pp)?;
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
            if op.size.is_some() && copied != planned_size {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "copy source changed size after compare: expected {planned_size}, read {copied}"
                    ),
                ));
            }
            pp.revise_total_bytes(planned_size, copied);
            let copied_hash = hasher.map(|hasher| hasher.finalize().to_hex().to_string());
            if let Some(expected) = expected_full_hash {
                if copied_hash.as_deref() != Some(expected) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("copy source content changed after compare: {}", op.path),
                    ));
                }
            }
            sh.checkpoint(pp)?;
            w.seal(sh.opt.fsync)?;

            // Length reconciliation (FFS's finalize check — it has caught corrupt
            // transfers in the wild): what the backend holds must equal what we sent
            let staged_len = w.staged_len()?;
            if staged_len != copied {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "staged file holds {staged_len} bytes but the copy stream carried {copied}"
                    ),
                ));
            }

            // Verification runs on the staged file: readback vs the copy stream — a failure never becomes the final file
            if sh.opt.verify {
                let expect = copied_hash
                    .as_deref()
                    .expect("verify=true initializes the copy-stream hasher");
                let mut rs = w.open_staged_read()?;
                let mut hh = blake3::Hasher::new();
                let mut b2 = vec![0u8; block];
                loop {
                    sh.checkpoint(pp)?;
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
                        format!(
                            "write verify failed: staged readback {got} != copy stream {expect}"
                        ),
                    ));
                }
            }

            // Archive the old file at the last moment before commit, so the window is a single rename
            if exec.stat(&op.path)?.is_some() {
                let relative = relative_path(&op.path)?;
                let newer = other_local.map(|root| (root, &relative));
                preserve(sh, op, exec, "overwritten", newer, pp)?;
            }
            sh.check_before_mutation()?;
            let report = w.commit_noreplace()?;

            apply_commit_report(sh, op, intended, report, pp)
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
            sh.check_before_mutation()?;
            Ok(exec.set_mode(&op.path, m)?)
        }
        Action::Move => {
            let from = op.from.as_deref().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("move operation is missing its source path: {}", op.path),
                )
            })?;

            // Claim the exact source pathname before creating a destination directory or staging
            // bytes. Later cleanup addresses only this hold, never `from`, so a new writer at the
            // original name cannot be deleted by this operation.
            let hold = claim_move_source(sh, exec.as_ref(), from, sh.opt.fsync)?;
            let publish = (|| -> std::io::Result<bool> {
                let moving = verify_move_evidence(sh, exec.as_ref(), &hold, op, pp)?;
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

                sh.check_before_mutation()?;
                match exec.rename_noreplace(&hold, &op.path) {
                    Ok(()) => Ok(false), // the hold itself became the destination
                    Err(error) if error.kind == VfsErrorKind::CrossDevice => {
                        if op.link.is_some() {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::Unsupported,
                                "cross-volume symlink move cannot be published atomically without a backend no-replace symlink primitive",
                            ));
                        }
                        copy_claimed_move(sh, exec.as_ref(), op, &hold, &moving, pp)?;
                        Ok(true) // destination published; caller must delete only the hold
                    }
                    Err(error) => Err(error.into()),
                }
            })();

            match publish {
                Ok(false) => {
                    // An already-open writer follows the claimed inode across rename. Recheck at
                    // its published name and put it back if it changed in the verification-to-
                    // rename window; never bless unverified bytes merely because no copy occurred.
                    if let Err(error) = verify_move_evidence(sh, exec.as_ref(), &op.path, op, pp) {
                        if sh.lease_lost() {
                            return Err(std::io::Error::other(
                                format!(
                                    "{error}; root-lock ownership was lost, so the published move was left at recoverable path '{}' instead of racing a rollback",
                                    op.path
                                ),
                            ));
                        }
                        return Err(rollback_claim(
                            exec.as_ref(),
                            &op.path,
                            from,
                            error,
                            sh.opt.fsync,
                        ));
                    }
                    sync_local_rename_parents(
                        exec.as_ref(),
                        &hold,
                        &op.path,
                        sh.opt.fsync,
                    )
                    .map_err(|error| {
                        std::io::Error::new(
                            error.kind(),
                            format!(
                                "move destination '{}' was published, but its directory entry could not be synced ({error})",
                                op.path
                            ),
                        )
                    })
                }
                Ok(true) => {
                    sh.check_before_mutation()?;
                    match exec.remove_file(&hold) {
                        Ok(()) => sync_local_parent(exec.as_ref(), &hold, sh.opt.fsync).map_err(
                            |error| {
                                std::io::Error::new(
                                    error.kind(),
                                    format!(
                                        "move destination '{}' was published and the claimed source removed, but the source directory could not be synced ({error})",
                                        op.path
                                    ),
                                )
                            },
                        ),
                        Err(error) => Err(std::io::Error::other(
                            format!(
                                "move destination '{}' was published, but the claimed source could not be removed ({error}); recoverable duplicate retained at '{hold}'",
                                op.path
                            ),
                        )),
                    }
                }
                Err(error) if sh.lease_lost() => Err(std::io::Error::other(
                    format!(
                        "{error}; root-lock ownership was lost, so the claimed original was left at recoverable path '{hold}' instead of racing a rollback"
                    ),
                )),
                Err(error) => Err(rollback_claim(
                    exec.as_ref(),
                    &hold,
                    from,
                    error,
                    sh.opt.fsync,
                )),
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
            match try_delete_dir_vfs(exec, &op.path, sh.opt.filter.as_ref(), || {
                sh.check_before_mutation()
            }) {
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
