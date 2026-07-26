//! apply：执行计划（本地/挂载模式）。
//! - 默认 DRY-RUN，--apply 才动手（GitDash 同款哲学：预览默认、绝不悄悄动手）
//! - **落盘一律原子**：写同目录临时文件 → fsync → rename（见 atomic.rs）。
//!   中断绝不会在最终路径留半截内容。
//! - 删除与被覆盖的文件先挪进本机回收目录 / `.version_syncDash`（可找回），不做原地销毁
//! - copy 后把 mtime 设成源表里的值，并**回读校正** —— 下次比对的相等判定依赖它

use crate::compare::{Action, Op, Side};
use filetime::FileTime;
use std::path::{Path, PathBuf};

/// 超过这个体积就不做内存内增量（避免把几 GB 读进内存）
const DELTA_MEM_CAP: u64 = 1024 * 1024 * 1024;

pub struct ApplyOptions {
    pub dry_run: bool,
    pub trash: Option<PathBuf>,
    pub verbose: bool,
    /// paranoid 严谨级：复制/更新后重读目标文件校验 blake3（FFS "verify copied files" 同款）
    pub verify: bool,
    /// 版本控制（可选）：被删/被覆盖的文件存进各 root 的 .version_syncDash/ 而非本机 trash
    pub versioning: bool,
    /// 临时文件 rename 前是否 fsync。默认 true；SMB 上嫌慢可关（自担风险）
    pub fsync: bool,
    /// 目录删除时用它判定"里面剩下的东西可不可以连带删"（syncthing 的 `(?d)`）
    pub filter: Option<crate::filter::PathFilter>,
    /// 本地/挂载盘的增量更新（详见 update_with_delta 的注释；默认关）
    pub delta: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        ApplyOptions {
            dry_run: true,
            trash: None,
            verbose: false,
            verify: false,
            versioning: false,
            fsync: true,
            filter: None,
            delta: false,
        }
    }
}

fn default_trash() -> PathBuf {
    crate::trash::trash_root().join(crate::table::now_ms().to_string())
}

fn to_native(rel: &str) -> String {
    if cfg!(windows) { rel.replace('/', "\\") } else { rel.to_string() }
}

fn move_to_trash(file: &Path, rel: &str, trash: &Path) -> std::io::Result<()> {
    let dest = trash.join(to_native(rel));
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    match std::fs::rename(file, &dest) {
        Ok(_) => Ok(()),
        Err(_) => {
            // 跨卷：退化为 copy+delete
            std::fs::copy(file, &dest)?;
            std::fs::remove_file(file)
        }
    }
}

fn set_mtime(path: &Path, mtime_ms: i64) {
    let ft = FileTime::from_unix_time(mtime_ms / 1000, ((mtime_ms % 1000) * 1_000_000) as u32);
    let _ = filetime::set_file_mtime(path, ft);
}

fn read_mtime_ms(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}
#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    // Windows 没有 unix 权限位。计划里带 mode 只会在 unix↔unix 之间产生，
    // 走到这里说明执行侧是 Windows —— 静默跳过而不是报错。
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
#[cfg(windows)]
fn create_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    // 文件链接失败（目标是目录/权限）再试目录链接；仍失败如实报错（Windows 需开发者模式或管理员）
    symlink_file(target, link).or_else(|_| symlink_dir(target, link))
}

fn exists_no_follow(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// 删除目录失败的分类结果（P0-4）。
/// 过去这里是 `Err(_) => Ok(())`：行为安全（不递归删），但**完全静默**——
/// 用户看到"0 错误"，可对面目录还在，下一轮比对又冒出同一条 DeleteDir，永远收敛不了。
enum DirOutcome {
    Removed,
    /// 本来就不在
    Absent,
    /// 非空，附上残留项样本与是否全部可删
    NotEmpty { sample: Vec<String> },
    Failed(std::io::Error),
}

fn try_delete_dir(dst: &Path, rel: &str, filter: Option<&crate::filter::PathFilter>) -> DirOutcome {
    match std::fs::remove_dir(dst) {
        Ok(_) => return DirOutcome::Removed,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return DirOutcome::Absent,
        Err(e) if e.kind() != std::io::ErrorKind::DirectoryNotEmpty && e.raw_os_error() != Some(145) && e.raw_os_error() != Some(39) => {
            // 145 = ERROR_DIR_NOT_EMPTY (Windows), 39 = ENOTEMPTY (unix)。
            // 其它错误（权限等）如实报告
            return DirOutcome::Failed(e);
        }
        Err(_) => {}
    }
    // 非空：看看剩下的是什么
    let mut sample = Vec::new();
    let mut all_deletable = true;
    let mut count = 0usize;
    for e in walkdir::WalkDir::new(dst).follow_links(false).min_depth(1).into_iter().flatten() {
        if !e.file_type().is_file() {
            continue;
        }
        count += 1;
        let child_rel = format!(
            "{}/{}",
            rel.trim_end_matches('/'),
            e.path().strip_prefix(dst).unwrap_or(e.path()).to_string_lossy().replace('\\', "/")
        );
        let deletable = filter.map(|f| f.is_deletable(&child_rel)).unwrap_or(false);
        if !deletable {
            all_deletable = false;
        }
        if sample.len() < 5 {
            sample.push(child_rel);
        }
    }
    if count == 0 {
        // 只剩子目录：递归删空目录树
        return match std::fs::remove_dir_all(dst) {
            Ok(_) => DirOutcome::Removed,
            Err(e) => DirOutcome::Failed(e),
        };
    }
    if all_deletable {
        // 剩下的全是 `(?d)` 可删项 / 原子写残骸 → 连带清掉（syncthing 同款语义）
        return match std::fs::remove_dir_all(dst) {
            Ok(_) => DirOutcome::Removed,
            Err(e) => DirOutcome::Failed(e),
        };
    }
    DirOutcome::NotEmpty { sample }
}

/// 增量更新（P1-1 步骤 B，opt-in）。
///
/// 做法：先把 dst 复制到同目录的临时文件（`fs::copy` 在 SMB2+ 上走服务端 copychunk、
/// 在支持 reflink 的本地 FS 上近似零成本），再只把**内容不同的 FastCDC 块**写进临时文件，
/// 最后 rename。同一条原子路径，不牺牲中断安全。
///
/// 取舍要说清楚：这条路要多读一遍 dst（远端读）换少写很多字节（远端写）。
/// SMB / WAN 上传写远比读贵时是净赚，对称链路上是打平——所以默认关闭，由 job 显式开。
/// 远程 pack 管线（v0.7 已有）才是增量收益最确定的地方。
fn update_with_delta(
    src: &Path,
    dst: &Path,
    staged: &mut crate::atomic::Staged,
) -> std::io::Result<Option<(u64, u64)>> {
    let (Ok(smd), Ok(dmd)) = (std::fs::metadata(src), std::fs::metadata(dst)) else {
        return Ok(None);
    };
    if smd.len() < crate::chunk::DELTA_MIN_SIZE
        || smd.len() > DELTA_MEM_CAP
        || dmd.len() > DELTA_MEM_CAP
    {
        return Ok(None);
    }
    let old = std::fs::read(dst)?;
    let new = std::fs::read(src)?;
    // 先把旧内容整体铺进临时文件（服务端复制 / reflink 的机会点）
    staged.write_at(0, &old)?;
    let old_chunks = crate::chunk::chunk_bytes(&old);
    let new_chunks = crate::chunk::chunk_bytes(&new);
    let have: std::collections::HashMap<&str, (u64, u32)> =
        old_chunks.iter().map(|c| (c.hash.as_str(), (c.off, c.len))).collect();
    let mut written = 0u64;
    for c in &new_chunks {
        let start = c.off as usize;
        let end = start + c.len as usize;
        // 块内容相同**且落在同一偏移**才能省下这一次写
        if let Some(&(off, len)) = have.get(c.hash.as_str()) {
            if off == c.off && len == c.len {
                continue;
            }
        }
        staged.write_at(c.off, &new[start..end])?;
        written += c.len as u64;
    }
    // 新文件更短时要截断掉尾巴
    if (new.len() as u64) < old.len() as u64 {
        let f = std::fs::OpenOptions::new().write(true).open(staged.path())?;
        f.set_len(new.len() as u64)?;
    }
    Ok(Some((written, new.len() as u64)))
}

pub fn apply(ops: &[Op], source_root: &Path, target_root: &Path, opt: &ApplyOptions) -> (u64, u64, u64) {
    // FFS dir_lock 思路：动手前锁两侧 root（带心跳），防两台机器同时 apply 同一目录
    let _lock_guard: Option<(crate::lock::RootLock, crate::lock::RootLock)> = if opt.dry_run {
        None
    } else {
        let ls = match crate::lock::RootLock::acquire(source_root) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("cannot lock source root: {e}");
                return (0, ops.len() as u64, 1);
            }
        };
        let lt = match crate::lock::RootLock::acquire(target_root) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("cannot lock target root: {e}");
                return (0, ops.len() as u64, 1);
            }
        };
        Some((ls, lt))
    };

    let trash = opt.trash.clone().unwrap_or_else(default_trash);
    let mut done = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;
    let mut ver_source: Option<crate::version::VersionWriter> = None;
    let mut ver_target: Option<crate::version::VersionWriter> = None;
    // P1-4：文件系统实际存下来的 mtime 与我们想要的不一致时（FAT 2 秒粒度、
    // 某些 SMB 服务端截断），记下 (ondisk, intended) 供下次扫描换算，
    // 而不是靠 ±2s 容差硬扛。syncthing 的 mtimeFS 同款做法。
    let mut mtime_fixes: Vec<(bool, String, i64, i64)> = Vec::new();
    let mut delta_saved = 0u64;

    for op in ops {
        let (exec_root, other_root) = match op.side {
            Side::Target => (target_root, source_root),
            Side::Source => (source_root, target_root),
        };
        let label = format!("[{}] {:?} {}", if op.side == Side::Target { "target" } else { "source" }, op.action, op.path);
        match op.action {
            Action::Conflict => {
                println!("CONFLICT  {} ({})", op.path, op.reason);
                skipped += 1;
                continue;
            }
            Action::Note => {
                println!("NOTE      {} ({} from={})", op.path, op.reason, op.from.clone().unwrap_or_default());
                skipped += 1;
                continue;
            }
            _ => {}
        }
        if opt.dry_run {
            if opt.verbose {
                println!("DRY  {label}  ({})", op.reason);
            }
            skipped += 1;
            continue;
        }

        // 被覆盖/被删的原件先留档（trash 或 .version_syncDash）
        let preserve = |dst: &Path, why: &str,
                            ver_source: &mut Option<crate::version::VersionWriter>,
                            ver_target: &mut Option<crate::version::VersionWriter>,
                            newer: Option<&Path>|
         -> std::io::Result<()> {
            if opt.versioning {
                let w = if op.side == Side::Source { ver_source } else { ver_target };
                if w.is_none() {
                    *w = Some(crate::version::VersionWriter::begin(exec_root)?);
                }
                w.as_mut().unwrap().preserve(&op.path, dst, newer, why)
            } else {
                move_to_trash(dst, &op.path, &trash)
            }
        };

        let res: std::io::Result<()> = (|| {
            let dst = exec_root.join(to_native(&op.path));
            match op.action {
                Action::Copy | Action::Update => {
                    if let Some(p) = dst.parent() {
                        std::fs::create_dir_all(p)?;
                    }
                    // symlink 操作：创建链接本身，不复制内容（链接是元数据，无所谓原子写）
                    if let Some(target) = &op.link {
                        if exists_no_follow(&dst) {
                            preserve(&dst, "overwritten", &mut ver_source, &mut ver_target, None)?;
                        }
                        return create_symlink(target, &dst);
                    }
                    let src = other_root.join(to_native(&op.path));

                    // ---- 原子落盘（P0-1）----
                    let mut staged = crate::atomic::Staged::create(&dst)?;
                    let mut used_delta = false;
                    if opt.delta && op.action == Action::Update && exists_no_follow(&dst) {
                        if let Some((written, total)) = update_with_delta(&src, &dst, &mut staged)? {
                            delta_saved += total.saturating_sub(written);
                            used_delta = true;
                        }
                    }
                    if !used_delta {
                        let mut fsrc = std::fs::File::open(&src)?;
                        staged.write_all_from(&mut fsrc)?;
                    }
                    staged.seal(opt.fsync)?;

                    // 校验在临时文件上做：不合格就根本不会成为最终文件
                    if opt.verify {
                        if let Some(expect) = &op.hash {
                            let mut hasher = blake3::Hasher::new();
                            hasher.update_mmap_rayon(staged.path())?;
                            let got = hasher.finalize().to_hex().to_string();
                            if &got != expect {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    format!("verify failed before commit: expected {expect}, got {got}"),
                                ));
                            }
                        }
                    }
                    // mtime / mode 也在临时文件上设好，rename 之后立刻就是终态
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
                    // 旧文件留档：放在 commit 前一刻，窗口只有一次 rename
                    if exists_no_follow(&dst) {
                        preserve(&dst, "overwritten", &mut ver_source, &mut ver_target, Some(&src))?;
                    }
                    staged.commit()?;

                    // ---- mtime 回读校正（P1-4）----
                    if let Some(want) = intended {
                        if let Some(got) = read_mtime_ms(&dst) {
                            if got != want {
                                mtime_fixes.push((op.side == Side::Source, op.path.clone(), got, want));
                            }
                        }
                    }
                    Ok(())
                }
                Action::Chmod => {
                    let m = op.mode.ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::InvalidInput, "chmod op without mode")
                    })?;
                    set_mode(&dst, m)
                }
                Action::Move => {
                    let from = exec_root.join(to_native(op.from.as_deref().unwrap_or_default()));
                    if let Some(p) = dst.parent() {
                        std::fs::create_dir_all(p)?;
                    }
                    match std::fs::rename(&from, &dst) {
                        Ok(_) => Ok(()),
                        Err(_) => {
                            // 跨卷退路：同样走原子写，中断不会留半截
                            let mut staged = crate::atomic::Staged::create(&dst)?;
                            let mut f = std::fs::File::open(&from)?;
                            staged.write_all_from(&mut f)?;
                            drop(f);
                            staged.seal(opt.fsync)?;
                            if let Some(mt) = read_mtime_ms(&from) {
                                set_mtime(staged.path(), mt);
                            }
                            staged.commit()?;
                            std::fs::remove_file(&from)
                        }
                    }
                }
                Action::Delete => {
                    // symlink_metadata：断链的 symlink exists() 会误报 false，这里不跟随
                    if exists_no_follow(&dst) {
                        preserve(&dst, "deleted", &mut ver_source, &mut ver_target, None)?;
                    }
                    Ok(())
                }
                Action::DeleteDir => {
                    // P0-4：分类汇报，不再静默吞掉
                    match try_delete_dir(&dst, &op.path, opt.filter.as_ref()) {
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
        })();
        match res {
            Ok(_) => {
                done += 1;
                if opt.verbose {
                    println!("OK   {label}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                // 目录留着不是错误（保护过滤中的文件是对的），但必须让人看见
                skipped += 1;
                println!("KEPT      {} ({e})", op.path);
            }
            Err(e) => {
                errors += 1;
                eprintln!("ERR  {label}: {e}");
            }
        }
    }

    if !opt.dry_run {
        if !mtime_fixes.is_empty() {
            let mut src_fix = Vec::new();
            let mut tgt_fix = Vec::new();
            for (is_source, rel, ondisk, intended) in mtime_fixes {
                if is_source {
                    src_fix.push((rel, ondisk, intended));
                } else {
                    tgt_fix.push((rel, ondisk, intended));
                }
            }
            if !src_fix.is_empty() {
                crate::scan::record_mtime_fixes(source_root, &src_fix);
            }
            if !tgt_fix.is_empty() {
                crate::scan::record_mtime_fixes(target_root, &tgt_fix);
            }
        }
        if delta_saved > 0 {
            println!("delta: {} not re-written", crate::preflight::human_bytes(delta_saved));
        }
        if let Some(w) = ver_source {
            let side_ops: Vec<crate::compare::Op> = ops.iter().filter(|o| o.side == Side::Source).cloned().collect();
            if let Ok(Some(id)) = w.finish(&side_ops) {
                println!("version saved: {} (id {id})", source_root.join(crate::version::STORE_DIR).display());
            }
        }
        if let Some(w) = ver_target {
            let side_ops: Vec<crate::compare::Op> = ops.iter().filter(|o| o.side == Side::Target).cloned().collect();
            if let Ok(Some(id)) = w.finish(&side_ops) {
                println!("version saved: {} (id {id})", target_root.join(crate::version::STORE_DIR).display());
            }
        }
        if !opt.versioning && done > 0 {
            println!("trash (deleted/overwritten files kept at): {}", trash.display());
        }
    }
    (done, skipped, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::{Action, Op, Side};

    fn tmproot(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("syncdash-apply-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn op(action: Action, path: &str) -> Op {
        Op {
            side: Side::Target,
            action,
            path: path.into(),
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "test".into(),
        }
    }

    fn opts(trash: PathBuf) -> ApplyOptions {
        ApplyOptions { dry_run: false, trash: Some(trash), fsync: false, ..Default::default() }
    }

    #[test]
    fn copy_lands_atomically_and_leaves_no_temp_files() {
        let base = tmproot("copy");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(s.join("a.txt"), b"hello").unwrap();

        let (done, _, errors) = apply(&[op(Action::Copy, "a.txt")], &s, &t, &opts(base.join("trash")));
        assert_eq!((done, errors), (1, 0));
        assert_eq!(std::fs::read(t.join("a.txt")).unwrap(), b"hello");
        let leftovers: Vec<_> = std::fs::read_dir(&t)
            .unwrap()
            .flatten()
            .filter(|e| crate::atomic::is_temp_name(&e.file_name().to_string_lossy()))
            .collect();
        assert!(leftovers.is_empty(), "no temp files may survive a successful apply");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn failed_update_leaves_the_original_intact() {
        // 源文件不存在 → 复制必然失败。目标必须原封不动，这正是原子写要保证的：
        // 过去 fs::copy 直接写目标，失败会留下截断内容，下一轮 sync 会把它反向传播回 source。
        let base = tmproot("fail");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join("keep.txt"), b"precious original").unwrap();

        let (done, _, errors) = apply(&[op(Action::Update, "keep.txt")], &s, &t, &opts(base.join("trash")));
        assert_eq!(done, 0);
        assert_eq!(errors, 1);
        assert_eq!(
            std::fs::read(t.join("keep.txt")).unwrap(),
            b"precious original",
            "a failed update must never damage the destination"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn update_preserves_old_content_in_trash() {
        let base = tmproot("trash");
        let (s, t, tr) = (base.join("s"), base.join("t"), base.join("trash"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(s.join("f.txt"), b"new").unwrap();
        std::fs::write(t.join("f.txt"), b"old").unwrap();

        let (done, _, errors) = apply(&[op(Action::Update, "f.txt")], &s, &t, &opts(tr.clone()));
        assert_eq!((done, errors), (1, 0));
        assert_eq!(std::fs::read(t.join("f.txt")).unwrap(), b"new");
        assert_eq!(std::fs::read(tr.join("f.txt")).unwrap(), b"old", "old version must be recoverable");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delete_dir_reports_kept_contents_instead_of_silence() {
        let base = tmproot("deldir");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(t.join("d")).unwrap();
        std::fs::write(t.join("d").join("protected.log"), b"x").unwrap();

        // 无 filter → 残留项不可删 → 计入 skipped 并打印原因，而不是假装成功
        let (done, skipped, errors) = apply(&[op(Action::DeleteDir, "d")], &s, &t, &opts(base.join("trash")));
        assert_eq!(done, 0, "the directory was not actually removed");
        assert_eq!(skipped, 1, "it must be reported, not silently counted as done");
        assert_eq!(errors, 0, "keeping a protected file is not an error");
        assert!(t.join("d").is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delete_dir_removes_when_leftovers_are_deletable() {
        let base = tmproot("deldir2");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(t.join("d")).unwrap();
        std::fs::write(t.join("d").join("cache.tmp"), b"x").unwrap();

        let mut o = opts(base.join("trash"));
        o.filter = Some(crate::filter::PathFilter::build_full(&[], &[], &["*/*.tmp".to_string()]));
        let (done, _, errors) = apply(&[op(Action::DeleteDir, "d")], &s, &t, &o);
        assert_eq!((done, errors), (1, 0));
        assert!(!t.join("d").exists(), "deletable leftovers must not block the directory removal");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delta_update_produces_identical_content() {
        let base = tmproot("delta");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        // 大于 DELTA_MIN_SIZE，且只有中间一小段不同
        let mut old = vec![0u8; 6 * 1024 * 1024];
        for (i, b) in old.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let mut new = old.clone();
        new[3_000_000..3_001_000].fill(0xAB);
        std::fs::write(s.join("big.bin"), &new).unwrap();
        std::fs::write(t.join("big.bin"), &old).unwrap();

        let mut o = opts(base.join("trash"));
        o.delta = true;
        let (done, _, errors) = apply(&[op(Action::Update, "big.bin")], &s, &t, &o);
        assert_eq!((done, errors), (1, 0));
        assert_eq!(std::fs::read(t.join("big.bin")).unwrap(), new, "delta path must be byte-exact");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delta_update_handles_shrinking_files() {
        let base = tmproot("delta2");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        let old = vec![7u8; 8 * 1024 * 1024];
        let new = vec![7u8; 5 * 1024 * 1024];
        std::fs::write(s.join("shrink.bin"), &new).unwrap();
        std::fs::write(t.join("shrink.bin"), &old).unwrap();

        let mut o = opts(base.join("trash"));
        o.delta = true;
        let (done, _, errors) = apply(&[op(Action::Update, "shrink.bin")], &s, &t, &o);
        assert_eq!((done, errors), (1, 0));
        assert_eq!(std::fs::read(t.join("shrink.bin")).unwrap().len(), new.len(), "tail must be truncated");
        let _ = std::fs::remove_dir_all(&base);
    }
}
