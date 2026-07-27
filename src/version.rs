//! 可选版本控制（v0.8）：job 里 `versioning = true` 后，apply 不再把被删/被覆盖的文件
//! 丢进本机 trash，而是存进 **该根目录自己的 `.version_syncDash/`** —— 历史跟着数据走，
//! 两台机器经 SMB 都能看见、都能恢复。
//!
//! 目录布局：
//!   <root>/.version_syncDash/
//!     index.jsonl                  一行一个版本 {id, ts_ms, host, ops, preserved, bytes}
//!     <id>/plan.jsonl              本次执行的指令清单（审计）
//!     <id>/manifest.json           保存条目：rel → whole|rdelta + 各 hash + 原 mtime/mode
//!     <id>/files/<rel>             原内容整存（小文件与被删除文件）
//!     <id>/rdelta/<rel>            反向补丁 blob（≥4MB 且有新内容可参照的被覆盖文件）
//!
//! 反向补丁：用 FastCDC 把"旧文件"表达为"新文件里已有的块 + blob 里的旧独有块"。
//! restore 时要求当前文件 hash == 记录的 new_hash，重组后校验 old_hash —— 三重保险。

use crate::chunk::RecipeStep;
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
    /// rdelta：重组时当前文件必须匹配的 hash
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

/// 一次 apply 在某个 root 上的版本写入器（惰性创建：真有保存动作才建目录）
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

    /// 保存一个即将被删/被覆盖的文件。new_content = 覆盖它的新内容（有则大文件走反向补丁）。
    /// 成功后原文件已从原位移走/可被覆盖。
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
        let try_rdelta = !is_link && old_size >= crate::chunk::DELTA_MIN_SIZE && new_content.map(|p| p.is_file()).unwrap_or(false);

        if try_rdelta {
            let old_data = std::fs::read(old_abs)?;
            let new_data = std::fs::read(new_content.unwrap())?;
            let old_hash = blake3::hash(&old_data).to_hex().to_string();
            let new_hash = blake3::hash(&new_data).to_hex().to_string();
            let new_chunks = crate::chunk::chunk_bytes(&new_data);
            let mut new_by_hash: std::collections::HashMap<&str, (u64, u32)> = std::collections::HashMap::new();
            for c in &new_chunks {
                new_by_hash.entry(c.hash.as_str()).or_insert((c.off, c.len));
            }
            let mut blob: Vec<u8> = Vec::new();
            let mut recipe: Vec<RecipeStep> = Vec::new();
            for c in crate::chunk::chunk_bytes(&old_data) {
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

    /// 收尾：写 plan/manifest/index。没保存任何东西则清掉空目录。
    pub fn finish(self, ops: &[crate::compare::Op]) -> std::io::Result<Option<String>> {
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
            host: crate::table::host_name(),
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

/// 保留最新 keep 个版本，其余删除。返回删掉的版本 id。
pub fn prune(root: &Path, keep: usize) -> std::io::Result<Vec<String>> {
    let mut all = list(root)?;
    all.sort_by_key(|l| l.ts_ms);
    let n = all.len().saturating_sub(keep);
    let drop: Vec<IndexLine> = all.drain(..n).collect();
    for d in &drop {
        let _ = std::fs::remove_dir_all(root.join(crate::foundation::names::VERSION_STORE_DIR).join(&d.id));
    }
    // 重写 index
    let idx_path = root.join(crate::foundation::names::VERSION_STORE_DIR).join("index.jsonl");
    let mut f = std::fs::File::create(&idx_path)?;
    for l in &all {
        writeln!(f, "{}", serde_json::to_string(l)?)?;
    }
    Ok(drop.into_iter().map(|d| d.id).collect())
}

/// 恢复：把某版本保存的文件放回原位（当前占位内容先进本机 trash）。
/// files 为空 = 全部；dry_run 只列出。返回 (restored, skipped, errors)。
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
            // 占位的当前文件挪进 trash（不销毁）
            if std::fs::symlink_metadata(&dst).is_ok() {
                let tp = trash.join(to_native(&e.rel));
                if let Some(par) = tp.parent() {
                    std::fs::create_dir_all(par)?;
                }
                if e.kind == "rdelta" {
                    // rdelta 需要当前文件作底——先校验再复制到 trash（保留原位做重组底本）
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
                let base = std::fs::read(&dst)?; // 当前文件 = new
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
        // 直接在字节层验证 反向补丁 的重组逻辑
        let mut old = vec![7u8; 6 * 1024 * 1024];
        let mut new = old.clone();
        for i in 3_000_000..3_004_096 {
            new[i] = 9;
        }
        new.extend_from_slice(&[1u8; 2048]);

        let new_chunks = crate::chunk::chunk_bytes(&new);
        let mut new_by_hash: std::collections::HashMap<&str, (u64, u32)> = std::collections::HashMap::new();
        for c in &new_chunks {
            new_by_hash.entry(c.hash.as_str()).or_insert((c.off, c.len));
        }
        let mut blob: Vec<u8> = Vec::new();
        let mut recipe: Vec<RecipeStep> = Vec::new();
        for c in crate::chunk::chunk_bytes(&old) {
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
