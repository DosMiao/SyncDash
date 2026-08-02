//! Low-level identity checks, claims, rollback, and commit reports for file transactions.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::foundation::names::TEMP_PREFIX;
use crate::foundation::path::RootRelativePath;
use crate::fs::vfs::error::VfsErrorKind;
use crate::fs::vfs::{VMeta, Vfs};
use crate::model::plan::{Op, Side};
use crate::obs::progress::PhaseProgress;

use super::schedule::Shared;

/// Read granularity for the local re-read in the delta lane, and therefore how often
/// cancel/pause is honoured mid-file. Same 8 MiB the scan lane reads in.
pub(in crate::pipeline::apply::execute) const READ_CHUNK: u64 = 8 * 1024 * 1024;
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

pub(in crate::pipeline::apply::execute) fn sync_local_parent(
    exec: &dyn Vfs,
    rel: &str,
    fsync: bool,
) -> std::io::Result<()> {
    if !fsync {
        return Ok(());
    }
    if let Some(root) = exec.local_root() {
        root.sync_parent(&relative_path(rel)?)?;
    }
    Ok(())
}

pub(in crate::pipeline::apply::execute) fn sync_local_rename_parents(
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

pub(in crate::pipeline::apply::execute) fn relative_path(
    relative: &str,
) -> std::io::Result<RootRelativePath> {
    RootRelativePath::try_from(relative)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

pub(in crate::pipeline::apply::execute) fn apply_commit_report(
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

pub(in crate::pipeline::apply::execute) fn claim_move_source(
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

pub(in crate::pipeline::apply::execute) fn rollback_claim(
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

pub(in crate::pipeline::apply::execute) fn verify_move_evidence(
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

pub(in crate::pipeline::apply::execute) fn copy_claimed_move(
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
