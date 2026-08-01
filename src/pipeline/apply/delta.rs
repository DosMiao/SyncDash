//! In-place patching for large updates: reuse the blocks the target already has and write only
//! what changed, instead of re-sending a file because a few bytes moved.

use std::path::Path;

use super::DELTA_MEM_CAP;

/// Delta update (P1-1 step B, opt-in).
///
/// How it works: first copy dst into a temp file in the same directory (`fs::copy` uses server-side copychunk
/// on SMB2+, and is near-free on local filesystems with reflink support), then write only the **FastCDC chunks
/// whose content differs** into that temp file, and rename at the end. Same atomic path, no loss of interruption safety.
///
/// The trade-off, stated plainly: this path reads dst one extra time (a remote read) to avoid writing a lot of bytes (remote writes).
/// It is a net win where writes cost far more than reads (SMB / WAN uplinks) and a wash on symmetric links — hence off by default, enabled explicitly per job.
/// The remote pack pipeline (already present in v0.7) is where the delta payoff is most certain.
pub(super) fn update_with_delta(
    src: &Path,
    dst: &Path,
    staged: &mut crate::fs::staged::Staged,
) -> std::io::Result<Option<(u64, u64, String)>> {
    let (Ok(smd), Ok(dmd)) = (std::fs::metadata(src), std::fs::metadata(dst)) else {
        return Ok(None);
    };
    if smd.len() < crate::model::chunk::DELTA_MIN_SIZE
        || smd.len() > DELTA_MEM_CAP
        || dmd.len() > DELTA_MEM_CAP
    {
        return Ok(None);
    }
    let old = std::fs::read(dst)?;
    let new = std::fs::read(src)?;
    // Lay the whole old content into the temp file first (the opening for server-side copy / reflink)
    staged.write_at(0, &old)?;
    let old_chunks = crate::model::chunk::chunk_bytes(&old);
    let new_chunks = crate::model::chunk::chunk_bytes(&new);
    let have: std::collections::HashMap<&str, (u64, u32)> = old_chunks
        .iter()
        .map(|c| (c.hash.as_str(), (c.off, c.len)))
        .collect();
    let mut written = 0u64;
    for c in &new_chunks {
        let start = c.off as usize;
        let end = start + c.len as usize;
        // Only when the chunk content matches **and it sits at the same offset** can this write be skipped
        if let Some(&(off, len)) = have.get(c.hash.as_str()) {
            if off == c.off && len == c.len {
                continue;
            }
        }
        staged.write_at(c.off, &new[start..end])?;
        written += c.len as u64;
    }
    // A shorter new file needs its tail truncated
    if (new.len() as u64) < old.len() as u64 {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(staged.path())?;
        f.set_len(new.len() as u64)?;
    }
    // The new content is right there in memory — hash it in full while we're at it, for post-copy verification against the readback from disk
    let h = blake3::hash(&new).to_hex().to_string();
    Ok(Some((written, new.len() as u64, h)))
}
