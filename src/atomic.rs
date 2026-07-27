//! 原子落盘（语义参照 syncthing `lib/osutil/atomic.go`，按语义重写）。
//!
//! 所有写入先落到**与目标同目录**的临时文件，数据 fsync 之后再 rename 到最终名。
//! 同卷 rename 是原子的：中断（断电/断网/Ctrl-C）只会留下一个临时文件，
//! 绝不会在最终路径上留下半截内容。
//!
//! 这条修的是一个真实的数据丢失路径：过去 `apply` 直接 `fs::copy(src, dst)`，
//! Update 写到一半断掉 → target 留下截断文件且 mtime 是新的 →
//! 下轮 sync 比对判成 "target-changed" → **把截断文件反向覆盖回 source**。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 临时文件前缀。扫描过滤器会排除它（见 filter::SELF_EXCLUDES，无条件生效），
/// 因此临时文件永远不会进快照表、不会被当成待同步内容。
pub const TEMP_PREFIX: &str = ".syncdash.tmp.";

/// 超过这个年龄的遗留临时文件视为上次中断的残骸，可安全清理。
pub const TEMP_LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;

/// 文件名（不含目录）是否是我们的临时文件
pub fn is_temp_name(file_name: &str) -> bool {
    file_name.starts_with(TEMP_PREFIX)
}

/// 相对路径（'/' 分隔）的末段是否是临时文件
pub fn is_temp_rel(rel: &str) -> bool {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    is_temp_name(base)
}

/// 暂存中的写入。Drop 时若未 commit 会自动删掉临时文件，
/// 因此任何提前 `?` 返回都不会留下垃圾。
pub struct Staged {
    tmp: PathBuf,
    dst: PathBuf,
    file: Option<std::fs::File>,
    committed: bool,
}

impl Staged {
    /// 在 dst 同目录创建临时文件。同目录 = 同卷，保证 commit 的 rename 是原子的
    /// （放系统 temp 目录就会退化成跨卷 copy，失去原子性）。
    pub fn create(dst: &Path) -> std::io::Result<Staged> {
        let dir = dst.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "destination has no parent directory")
        })?;
        let base = dst.file_name().and_then(|s| s.to_str()).unwrap_or("out");
        // 带 pid：同一目录下两个 syncdash 进程各写各的，互不踩踏
        let tmp = dir.join(format!("{TEMP_PREFIX}{base}.{}", std::process::id()));
        // 上一次中断可能留下同名残骸
        if tmp.exists() {
            std::fs::remove_file(&tmp)?;
        }
        let file = std::fs::File::create(&tmp)?;
        Ok(Staged { tmp, dst: dst.to_path_buf(), file: Some(file), committed: false })
    }

    /// 临时文件路径（校验 hash / 设 mtime 都对它做，commit 后才成为最终文件）
    pub fn path(&self) -> &Path {
        &self.tmp
    }

    /// 把 reader 的内容全部写进来
    pub fn write_all_from(&mut self, r: &mut dyn std::io::Read) -> std::io::Result<u64> {
        let f = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "staged file already sealed"))?;
        std::io::copy(r, f)
    }

    /// 在指定偏移写入一段数据（增量传输：只补不同的块）
    pub fn write_at(&mut self, offset: u64, buf: &[u8]) -> std::io::Result<()> {
        use std::io::Seek;
        let f = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "staged file already sealed"))?;
        f.seek(std::io::SeekFrom::Start(offset))?;
        f.write_all(buf)
    }

    /// v0.9 M1：唯一的流式复制环——1MiB 缓冲，每块之后回调
    /// （进度计数＋取消/暂停检查点挂这里）。on_chunk 返回 Err 即中止：
    /// Drop 清掉临时文件，最终路径纹丝不动。
    /// 同一个环同时服务：字节级进度（M1）、原子落盘（P0-1）、将来的 write_at 增量（P1-1B）。
    pub fn copy_from(
        &mut self,
        src: &Path,
        on_chunk: &mut dyn FnMut(u64) -> std::io::Result<()>,
    ) -> std::io::Result<u64> {
        use std::io::Read;
        let f = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "staged file already sealed"))?;
        let mut reader = std::fs::File::open(src)?;
        let mut buf = vec![0u8; 1024 * 1024];
        let mut total = 0u64;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            f.write_all(&buf[..n])?;
            total += n as u64;
            on_chunk(n as u64)?;
        }
        Ok(total)
    }

    /// 数据落盘并关闭句柄。
    /// **必须在 commit 前调用**：Windows 上 rename 目标/源有打开的写句柄会失败。
    pub fn seal(&mut self, fsync: bool) -> std::io::Result<()> {
        if let Some(mut f) = self.file.take() {
            f.flush()?;
            if fsync {
                // SMB 上 fsync 可能较慢，但这正是"rename 之后内容真的在盘上"的保证。
                // 需要极致吞吐时可由 job 的 fsync=false 关掉（自担风险）。
                f.sync_all()?;
            }
        }
        Ok(())
    }

    /// 原子替换最终文件。调用前必须 seal。
    pub fn commit(mut self) -> std::io::Result<()> {
        if self.file.is_some() {
            self.seal(true)?;
        }
        std::fs::rename(&self.tmp, &self.dst)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.committed {
            self.file.take(); // 先关句柄，Windows 才删得掉
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

/// 清理 root 下超龄的遗留临时文件。返回删除的个数。
/// 只在明确处于写入流程时调用（scan 默认不做，避免在只读挂载上意外写）。
pub fn sweep_stale_temps(root: &Path, now_ms: i64) -> u64 {
    let mut n = 0u64;
    let walker = walkdir::WalkDir::new(root).follow_links(false).into_iter();
    for item in walker.flatten() {
        if !item.file_type().is_file() {
            continue;
        }
        let name = item.file_name().to_string_lossy();
        if !is_temp_name(&name) {
            continue;
        }
        let age_ok = item
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| now_ms - d.as_millis() as i64 > TEMP_LIFETIME_MS)
            .unwrap_or(false);
        if age_ok && std::fs::remove_file(item.path()).is_ok() {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("syncdash-atomic-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn commit_replaces_atomically() {
        let d = tmpdir("commit");
        let dst = d.join("f.txt");
        std::fs::write(&dst, b"old").unwrap();
        let mut s = Staged::create(&dst).unwrap();
        s.write_all_from(&mut &b"new content"[..]).unwrap();
        s.seal(true).unwrap();
        // commit 之前，最终路径上仍是旧内容 —— 这正是原子性的意义
        assert_eq!(std::fs::read(&dst).unwrap(), b"old");
        s.commit().unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"new content");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn abandoned_stage_leaves_destination_untouched() {
        let d = tmpdir("abandon");
        let dst = d.join("f.txt");
        std::fs::write(&dst, b"original").unwrap();
        {
            let mut s = Staged::create(&dst).unwrap();
            s.write_all_from(&mut &b"half written..."[..]).unwrap();
            // 不 commit 就 drop —— 模拟中途失败
        }
        assert_eq!(std::fs::read(&dst).unwrap(), b"original", "dst must never see partial content");
        // 临时文件也不该留下
        let leftovers: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter(|e| is_temp_name(&e.file_name().to_string_lossy()))
            .collect();
        assert!(leftovers.is_empty(), "Drop must clean up the temp file");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn new_file_absent_until_commit() {
        let d = tmpdir("newfile");
        let dst = d.join("brand-new.bin");
        {
            let mut s = Staged::create(&dst).unwrap();
            s.write_all_from(&mut &b"xyz"[..]).unwrap();
            assert!(!dst.exists(), "destination must not appear before commit");
        }
        assert!(!dst.exists(), "abandoned write must not create the destination");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_at_patches_offsets() {
        let d = tmpdir("patch");
        let dst = d.join("p.bin");
        let mut s = Staged::create(&dst).unwrap();
        s.write_all_from(&mut &b"AAAAAAAAAA"[..]).unwrap();
        s.write_at(3, b"ZZ").unwrap();
        s.seal(false).unwrap();
        s.commit().unwrap();
        let mut got = Vec::new();
        std::fs::File::open(&dst).unwrap().read_to_end(&mut got).unwrap();
        assert_eq!(&got, b"AAAZZAAAAA");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn copy_from_counts_and_aborts() {
        let d = tmpdir("copyfrom");
        let src = d.join("src.bin");
        std::fs::write(&src, vec![7u8; 3 * 1024 * 1024 + 123]).unwrap();
        let dst = d.join("dst.bin");
        // 正常：块回调字节和 == 文件长
        {
            let mut s = Staged::create(&dst).unwrap();
            let mut seen = 0u64;
            let total = s.copy_from(&src, &mut |n| { seen += n; Ok(()) }).unwrap();
            assert_eq!(total, 3 * 1024 * 1024 + 123);
            assert_eq!(seen, total);
            s.seal(false).unwrap();
            s.commit().unwrap();
        }
        assert_eq!(std::fs::metadata(&dst).unwrap().len(), 3 * 1024 * 1024 + 123);
        // 中止：第一块后喊停 → dst 不变、零临时残留
        std::fs::write(&dst, b"keep me").unwrap();
        {
            let mut s = Staged::create(&dst).unwrap();
            let res = s.copy_from(&src, &mut |_| Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "stop")));
            assert!(res.is_err());
        }
        assert_eq!(std::fs::read(&dst).unwrap(), b"keep me");
        let leftovers: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter(|e| is_temp_name(&e.file_name().to_string_lossy()))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn temp_name_detection() {
        assert!(is_temp_rel("a/b/.syncdash.tmp.x.1234"));
        assert!(!is_temp_rel("a/b/normal.txt"));
        assert!(is_temp_name(".syncdash.tmp.foo"));
    }
}
