//! Optional versioning (v0.8): with `versioning = true` in the job, apply no longer throws deleted or
//! overwritten files into the local trash but stores them in **that root's own `.version_syncDash/`** ——
//! history travels with the data, so both machines see it over SMB and either can restore from it.
//!
//! Directory layout:
//!   <root>/.version_syncDash/
//!     index.jsonl                  one line per version {id, ts_ms, host, ops, preserved, bytes}
//!     <id>/plan.jsonl              the op list this run executed (audit trail)
//!     <id>/manifest.json           preserved entries: rel → whole|rdelta + the hashes + original mtime/mode
//!     <id>/files/<rel>             whole-file copy of the original (small files and deleted files)
//!     <id>/rdelta/<rel>            reverse-patch blob (overwritten files ≥4MB whose new content can serve as a reference)
//!
//! Reverse patch: FastCDC expresses the "old file" as "chunks already present in the new file + the old-only chunks in the blob".
//! restore requires the current file's hash == the recorded new_hash and verifies old_hash after reassembly —— three checks in series.

use crate::model::chunk::RecipeStep;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone)]
pub struct PreservedEntry {
    pub rel: String,
    /// "whole" | "rdelta"
    pub kind: String,
    /// deleted | overwritten
    pub why: String,
    pub old_hash: String,
    pub old_size: u64,
    pub old_mtime_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub old_mode: Option<u32>,
    /// rdelta: the hash the current file must match at reassembly time
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub new_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub recipe: Option<Vec<RecipeStep>>,
}

#[derive(Serialize, Deserialize)]
pub struct VersionManifest {
    pub id: String,
    pub ts_ms: u64,
    pub host: String,
    pub entries: Vec<PreservedEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct IndexLine {
    pub id: String,
    pub ts_ms: u64,
    pub host: String,
    pub ops: u64,
    pub preserved: u64,
    pub bytes: u64,
}

fn to_native(rel: &str) -> String {
    if cfg!(windows) { rel.replace('/', "\\") } else { rel.to_string() }
}

fn mtime_ms_of(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn hash_file(p: &Path) -> std::io::Result<String> {
    let mut h = blake3::Hasher::new();
    h.update_mmap_rayon(p)?;
    Ok(h.finalize().to_hex().to_string())
}

/// Version writer for one apply run against one root (lazily created: the directory appears only once something is really preserved)
pub struct VersionWriter {
    root: PathBuf,
    vdir: PathBuf,
    id: String,
    entries: Vec<PreservedEntry>,
    bytes: u64,
}

impl VersionWriter {
    pub fn begin(root: &Path) -> std::io::Result<VersionWriter> {
        let id = format!("{}-{}", crate::foundation::time::now_ms(), std::process::id());
        let vdir = root.join(crate::foundation::names::VERSION_STORE_DIR).join(&id);
        std::fs::create_dir_all(&vdir)?;
        Ok(VersionWriter { root: root.to_path_buf(), vdir, id, entries: Vec::new(), bytes: 0 })
    }

    /// Preserve a file that is about to be deleted or overwritten. new_content = the new content replacing it (when present, large files go through a reverse patch).
    /// On success the original has been moved out of its old spot / is free to be overwritten.
    pub fn preserve(&mut self, rel: &str, old_abs: &Path, new_content: Option<&Path>, why: &str) -> std::io::Result<()> {
        let md = std::fs::symlink_metadata(old_abs)?;
        let old_size = md.len();
        let old_mtime_ms = mtime_ms_of(&md);
        #[cfg(unix)]
        let old_mode = {
            use std::os::unix::fs::MetadataExt;
            Some(md.mode() & 0o7777)
        };
        #[cfg(not(unix))]
        let old_mode: Option<u32> = None;

        let is_link = md.file_type().is_symlink();
        let try_rdelta = !is_link && old_size >= crate::model::chunk::DELTA_MIN_SIZE && new_content.map(|p| p.is_file()).unwrap_or(false);

        if try_rdelta {
            let old_data = std::fs::read(old_abs)?;
            let new_data = std::fs::read(new_content.unwrap())?;
            let old_hash = blake3::hash(&old_data).to_hex().to_string();
            let new_hash = blake3::hash(&new_data).to_hex().to_string();
            let new_chunks = crate::model::chunk::chunk_bytes(&new_data);
            let mut new_by_hash: std::collections::HashMap<&str, (u64, u32)> = std::collections::HashMap::new();
            for c in &new_chunks {
                new_by_hash.entry(c.hash.as_str()).or_insert((c.off, c.len));
            }
            let mut blob: Vec<u8> = Vec::new();
            let mut recipe: Vec<RecipeStep> = Vec::new();
            for c in crate::model::chunk::chunk_bytes(&old_data) {
                if let Some(&(noff, nlen)) = new_by_hash.get(c.hash.as_str()) {
                    recipe.push(RecipeStep { s: "base".into(), off: noff, len: nlen });
                } else {
                    let off = blob.len() as u64;
                    blob.extend_from_slice(&old_data[c.off as usize..(c.off + c.len as u64) as usize]);
                    recipe.push(RecipeStep { s: "blob".into(), off, len: c.len });
                }
            }
            let bp = self.vdir.join("rdelta").join(to_native(rel));
            if let Some(par) = bp.parent() {
                std::fs::create_dir_all(par)?;
            }
            std::fs::write(&bp, &blob)?;
            self.bytes += blob.len() as u64;
            self.entries.push(PreservedEntry {
                rel: rel.to_string(),
                kind: "rdelta".into(),
                why: why.into(),
                old_hash,
                old_size,
                old_mtime_ms,
                old_mode,
                new_hash: Some(new_hash),
                recipe: Some(recipe),
            });
            std::fs::remove_file(old_abs)?;
        } else {
            let old_hash = if is_link { String::new() } else { hash_file(old_abs)? };
            let fp = self.vdir.join("files").join(to_native(rel));
            if let Some(par) = fp.parent() {
                std::fs::create_dir_all(par)?;
            }
            match std::fs::rename(old_abs, &fp) {
                Ok(_) => {}
                Err(_) => {
                    std::fs::copy(old_abs, &fp)?;
                    std::fs::remove_file(old_abs)?;
                }
            }
            self.bytes += old_size;
            self.entries.push(PreservedEntry {
                rel: rel.to_string(),
                kind: "whole".into(),
                why: why.into(),
                old_hash,
                old_size,
                old_mtime_ms,
                old_mode,
                new_hash: None,
                recipe: None,
            });
        }
        Ok(())
    }

    pub fn has_content(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Wrap up: write plan/manifest/index. If nothing was preserved, clear away the empty directory.
    pub fn finish(self, ops: &[crate::model::plan::Op]) -> std::io::Result<Option<String>> {
        if self.entries.is_empty() {
            let _ = std::fs::remove_dir_all(&self.vdir);
            return Ok(None);
        }
        let mut pf = std::fs::File::create(self.vdir.join("plan.jsonl"))?;
        for op in ops {
            writeln!(pf, "{}", serde_json::to_string(op)?)?;
        }
        let manifest = VersionManifest {
            id: self.id.clone(),
            ts_ms: crate::foundation::time::now_ms(),
            host: crate::model::table::host_name(),
            entries: self.entries.clone(),
        };
        std::fs::write(self.vdir.join("manifest.json"), serde_json::to_vec_pretty(&manifest)?)?;
        let idx = IndexLine {
            id: self.id.clone(),
            ts_ms: manifest.ts_ms,
            host: manifest.host.clone(),
            ops: ops.len() as u64,
            preserved: self.entries.len() as u64,
            bytes: self.bytes,
        };
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(crate::foundation::names::VERSION_STORE_DIR).join("index.jsonl"))?;
        writeln!(f, "{}", serde_json::to_string(&idx)?)?;
        Ok(Some(self.id))
    }
}

pub fn list(root: &Path) -> std::io::Result<Vec<IndexLine>> {
    let p = root.join(crate::foundation::names::VERSION_STORE_DIR).join("index.jsonl");
    let mut out = Vec::new();
    if let Ok(text) = std::fs::read_to_string(&p) {
        for line in text.lines() {
            if let Ok(l) = serde_json::from_str::<IndexLine>(line.trim()) {
                out.push(l);
            }
        }
    }
    Ok(out)
}

/// Keep the newest `keep` versions and delete the rest. Returns the ids of the deleted versions.
pub fn prune(root: &Path, keep: usize) -> std::io::Result<Vec<String>> {
    let mut all = list(root)?;
    all.sort_by_key(|l| l.ts_ms);
    let n = all.len().saturating_sub(keep);
    let drop: Vec<IndexLine> = all.drain(..n).collect();
    for d in &drop {
        let _ = std::fs::remove_dir_all(root.join(crate::foundation::names::VERSION_STORE_DIR).join(&d.id));
    }
    // Rewrite the index
    let idx_path = root.join(crate::foundation::names::VERSION_STORE_DIR).join("index.jsonl");
    let mut f = std::fs::File::create(&idx_path)?;
    for l in &all {
        writeln!(f, "{}", serde_json::to_string(l)?)?;
    }
    Ok(drop.into_iter().map(|d| d.id).collect())
}

/// Restore: put a version's preserved files back where they came from (whatever currently occupies the spot goes into the local trash first).
/// Empty `files` = all of them; dry_run only lists. Returns (restored, skipped, errors).
pub fn restore(root: &Path, version: &str, files: &[String], dry_run: bool) -> std::io::Result<(u64, u64, u64)> {
    let vdir = root.join(crate::foundation::names::VERSION_STORE_DIR).join(version);
    let mani: VersionManifest = serde_json::from_slice(&std::fs::read(vdir.join("manifest.json"))?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad manifest: {e}")))?;
    let trash = std::env::temp_dir().join(format!("syncdash-restore-displaced-{}", crate::foundation::time::now_ms()));
    let mut restored = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;
    for e in &mani.entries {
        if !files.is_empty() && !files.iter().any(|f| f == &e.rel) {
            skipped += 1;
            continue;
        }
        if dry_run {
            println!("DRY  restore {} ({} , {} B, {})", e.rel, e.kind, e.old_size, e.why);
            skipped += 1;
            continue;
        }
        let dst = root.join(to_native(&e.rel));
        let res: std::io::Result<()> = (|| {
            if let Some(par) = dst.parent() {
                std::fs::create_dir_all(par)?;
            }
            // Move the current occupant into the trash (never destroy it)
            if std::fs::symlink_metadata(&dst).is_ok() {
                let tp = trash.join(to_native(&e.rel));
                if let Some(par) = tp.parent() {
                    std::fs::create_dir_all(par)?;
                }
                if e.kind == "rdelta" {
                    // rdelta needs the current file as its base — verify first, then copy to trash (the original stays in place as the reassembly base)
                    let cur_hash = hash_file(&dst)?;
                    if Some(cur_hash.as_str()) != e.new_hash.as_deref() {
                        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "current file no longer matches recorded new_hash"));
                    }
                    std::fs::copy(&dst, &tp)?;
                } else {
                    std::fs::rename(&dst, &tp).or_else(|_| std::fs::copy(&dst, &tp).map(|_| ()).and_then(|_| std::fs::remove_file(&dst)))?;
                }
            } else if e.kind == "rdelta" {
                return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "rdelta base (current file) is gone"));
            }
            if e.kind == "rdelta" {
                let blob = std::fs::read(vdir.join("rdelta").join(to_native(&e.rel)))?;
                let base = std::fs::read(&dst)?; // the current file is the new one
                let mut out: Vec<u8> = Vec::with_capacity(e.old_size as usize);
                if let Some(recipe) = &e.recipe {
                    for st in recipe {
                        let (src, off, len) = (&st.s, st.off as usize, st.len as usize);
                        let slice = if src == "base" { base.get(off..off + len) } else { blob.get(off..off + len) };
                        match slice {
                            Some(s) => out.extend_from_slice(s),
                            None => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "recipe out of range")),
                        }
                    }
                }
                let got = blake3::hash(&out).to_hex().to_string();
                if got != e.old_hash {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "reconstructed old content hash mismatch"));
                }
                std::fs::write(&dst, &out)?;
            } else {
                let src = vdir.join("files").join(to_native(&e.rel));
                std::fs::copy(&src, &dst)?;
            }
            let ft = filetime::FileTime::from_unix_time(e.old_mtime_ms / 1000, ((e.old_mtime_ms % 1000) * 1_000_000) as u32);
            let _ = filetime::set_file_mtime(&dst, ft);
            #[cfg(unix)]
            if let Some(mode) = e.old_mode {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode));
            }
            Ok(())
        })();
        match res {
            Ok(_) => {
                restored += 1;
                println!("OK   restored {}", e.rel);
            }
            Err(err) => {
                errors += 1;
                crate::log_error!("version", "ERR  restore {}: {err}", e.rel);
            }
        }
    }
    if restored > 0 && trash.exists() {
        println!("displaced current files kept at: {}", trash.display());
    }
    Ok((restored, skipped, errors))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rdelta_roundtrip_bytes() {
        // Verify the reverse-patch reassembly logic directly at the byte level
        let mut old = vec![7u8; 6 * 1024 * 1024];
        let mut new = old.clone();
        for i in 3_000_000..3_004_096 {
            new[i] = 9;
        }
        new.extend_from_slice(&[1u8; 2048]);

        let new_chunks = crate::model::chunk::chunk_bytes(&new);
        let mut new_by_hash: std::collections::HashMap<&str, (u64, u32)> = std::collections::HashMap::new();
        for c in &new_chunks {
            new_by_hash.entry(c.hash.as_str()).or_insert((c.off, c.len));
        }
        let mut blob: Vec<u8> = Vec::new();
        let mut recipe: Vec<RecipeStep> = Vec::new();
        for c in crate::model::chunk::chunk_bytes(&old) {
            if let Some(&(noff, nlen)) = new_by_hash.get(c.hash.as_str()) {
                recipe.push(RecipeStep { s: "base".into(), off: noff, len: nlen });
            } else {
                let off = blob.len() as u64;
                blob.extend_from_slice(&old[c.off as usize..(c.off + c.len as u64) as usize]);
                recipe.push(RecipeStep { s: "blob".into(), off, len: c.len });
            }
        }
        assert!(blob.len() < old.len() / 4, "rdelta blob should be much smaller (got {} of {})", blob.len(), old.len());

        let mut rebuilt: Vec<u8> = Vec::new();
        for st in &recipe {
            let (off, len) = (st.off as usize, st.len as usize);
            let s = if st.s == "base" { &new[off..off + len] } else { &blob[off..off + len] };
            rebuilt.extend_from_slice(s);
        }
        assert_eq!(blake3::hash(&rebuilt), blake3::hash(&old));
        old.clear(); // silence unused-mut lint paranoia
    }
}
