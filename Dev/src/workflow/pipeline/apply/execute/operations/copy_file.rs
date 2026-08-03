//! Copy and Update through delta-aware or generic staged publication.

use std::sync::atomic::Ordering;

use crate::model::plan::{Action, Op, Side};
use crate::obs::progress::PhaseProgress;

use super::super::delta::stage_delta_update;
use super::super::file_transaction::{apply_commit_report, relative_path, READ_CHUNK};
use super::super::preservation::preserve;
use super::super::schedule::Shared;

pub(in crate::pipeline::apply::execute) fn execute(
    sh: &Shared<'_>,
    op: &Op,
    pp: &PhaseProgress<'_>,
) -> std::io::Result<()> {
    use crate::fs::vfs::WriteHint;

    let (exec, other) = sh.exec_other(&op.side);
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
                    stage_delta_update(oroot, eroot, &relative, &mut staged, || sh.checkpoint(pp))?
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
                        crate::model::digest::verifiable_digest(expected)
                            .is_some_and(|full| full != delta.output_hash)
                    }) {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("copy source content changed after compare: {}", op.path),
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
                        let mut buf = vec![0u8; delta.output_size.clamp(1, READ_CHUNK) as usize];
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
                    let mtime_error = intended.and_then(|mtime| staged.set_mtime(mtime).err());
                    #[cfg(unix)]
                    if let Some(mode) = op.mode {
                        staged.set_mode(mode)?;
                    }
                    if sh.opt.fsync {
                        staged.sync_file()?;
                    }
                    match eroot.metadata_path(&relative) {
                        Ok(_) => {
                            preserve(sh, op, exec, "overwritten", Some((oroot, &relative)), pp)?;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                    sh.check_before_mutation()?;
                    staged.commit_noreplace()?;
                    if let Some(want) = intended {
                        let metadata = eroot.metadata_path(&relative)?;
                        let got =
                            crate::foundation::time::systime_ms(metadata.modified()?.into_std());
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
                                        op.side.as_str(),
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
        .filter(|expected| !crate::model::digest::is_sampled_digest(expected));
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
            format!("staged file holds {staged_len} bytes but the copy stream carried {copied}"),
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
                format!("write verify failed: staged readback {got} != copy stream {expect}"),
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
