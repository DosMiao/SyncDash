//! apply：执行计划（本地/挂载模式）。
//! - 默认 DRY-RUN，--apply 才动手（GitDash 同款哲学：预览默认、绝不悄悄动手）
//! - **落盘一律原子**：写同目录临时文件 → fsync → rename（见 atomic.rs）。
//!   中断绝不会在最终路径留半截内容。
//! - 删除与被覆盖的文件先挪进本机回收目录 / `.version_syncDash`（可找回），不做原地销毁
//! - copy 后把 mtime 设成源表里的值，并**回读校正** —— 下次比对的相等判定依赖它
//! - v0.9 M1：执行分五个**串行相位**，其中 Copy/Update 相位内部并行
//!   （唯一 I/O 重类；宽度 = `ApplyOptions::parallel`）。不信输入顺序——桌面翻向的
//!   ops 会破坏计划的 rank 排序，apply 自己重新分类分相。

use crate::compare::{Action, Op, Side};
use crate::progress::{ApplyOutcome, Phase, PhaseProgress, RunCtx};
use filetime::FileTime;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

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
    /// Copy/Update 相位的并行宽度（1 = 顺序）。默认 4：SMB 上 2-4 条流基本吃满
    /// 上行带宽，再宽只会加剧对端排队。clamp 到 1..=16。
    pub parallel: usize,
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
            parallel: 4,
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

/// 工作线程共享的执行环境。计数器全原子、写入器上锁——worker 只借 &Shared。
struct Shared<'a> {
    opt: &'a ApplyOptions,
    ctx: &'a RunCtx,
    source_root: &'a Path,
    target_root: &'a Path,
    trash: PathBuf,
    ver_source: Mutex<Option<crate::version::VersionWriter>>,
    ver_target: Mutex<Option<crate::version::VersionWriter>>,
    // P1-4：文件系统实际存下来的 mtime 与我们想要的不一致时（FAT 2 秒粒度、
    // 某些 SMB 服务端截断），记下 (ondisk, intended) 供下次扫描换算，
    // 而不是靠 ±2s 容差硬扛。syncthing 的 mtimeFS 同款做法。
    mtime_fixes: Mutex<Vec<(bool, String, i64, i64)>>,
    delta_saved: AtomicU64,
}

#[derive(Default)]
struct Counters {
    done: AtomicU64,
    skipped: AtomicU64,
    errors: AtomicU64,
}

/// 被覆盖/被删的原件先留档（trash 或 .version_syncDash）
fn preserve(sh: &Shared, op: &Op, exec_root: &Path, dst: &Path, why: &str, newer: Option<&Path>) -> std::io::Result<()> {
    if sh.opt.versioning {
        let slot = if op.side == Side::Source { &sh.ver_source } else { &sh.ver_target };
        let mut w = slot.lock().unwrap();
        if w.is_none() {
            *w = Some(crate::version::VersionWriter::begin(exec_root)?);
        }
        w.as_mut().unwrap().preserve(&op.path, dst, newer, why)
    } else {
        move_to_trash(dst, &op.path, &sh.trash)
    }
}

/// 执行单个 op。取消/暂停经 `pp.checkpoint()` 在块边界响应；
/// 取消返回 Interrupted，Staged::Drop 保证终点无残留。
fn exec_op(sh: &Shared, op: &Op, pp: &PhaseProgress) -> std::io::Result<()> {
    let (exec_root, other_root) = match op.side {
        Side::Target => (sh.target_root, sh.source_root),
        Side::Source => (sh.source_root, sh.target_root),
    };
    let dst = exec_root.join(to_native(&op.path));
    match op.action {
        Action::Copy | Action::Update => {
            if let Some(p) = dst.parent() {
                std::fs::create_dir_all(p)?;
            }
            // symlink 操作：创建链接本身，不复制内容（链接是元数据，无所谓原子写）
            if let Some(target) = &op.link {
                if exists_no_follow(&dst) {
                    preserve(sh, op, exec_root, &dst, "overwritten", None)?;
                }
                return create_symlink(target, &dst);
            }
            let src = other_root.join(to_native(&op.path));

            // ---- 原子落盘（P0-1）----
            let mut staged = crate::atomic::Staged::create(&dst)?;
            let mut used_delta = false;
            if sh.opt.delta && op.action == Action::Update && exists_no_follow(&dst) {
                if let Some((written, total)) = update_with_delta(&src, &dst, &mut staged)? {
                    sh.delta_saved.fetch_add(total.saturating_sub(written), Ordering::Relaxed);
                    // 进度按"这个文件完成了"记账（与 bytes_total 同一口径）；省下的字节另行汇报
                    pp.add_bytes(total, &op.path);
                    used_delta = true;
                }
            }
            if !used_delta {
                staged.copy_from(&src, &mut |n| {
                    pp.add_bytes(n, &op.path);
                    pp.checkpoint()
                })?;
            }
            staged.seal(sh.opt.fsync)?;

            // 校验在临时文件上做：不合格就根本不会成为最终文件
            if sh.opt.verify {
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
                preserve(sh, op, exec_root, &dst, "overwritten", Some(&src))?;
            }
            staged.commit()?;

            // ---- mtime 回读校正（P1-4）----
            if let Some(want) = intended {
                if let Some(got) = read_mtime_ms(&dst) {
                    if got != want {
                        sh.mtime_fixes.lock().unwrap().push((op.side == Side::Source, op.path.clone(), got, want));
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
                    // 跨卷退路：同样走原子写，中断不会留半截。
                    // Move 的字节不在 bytes_total 里，这里只挂检查点、不计字节。
                    let mut staged = crate::atomic::Staged::create(&dst)?;
                    staged.copy_from(&from, &mut |_| pp.checkpoint())?;
                    staged.seal(sh.opt.fsync)?;
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
                preserve(sh, op, exec_root, &dst, "deleted", None)?;
            }
            Ok(())
        }
        Action::DeleteDir => {
            // P0-4：分类汇报，不再静默吞掉
            match try_delete_dir(&dst, &op.path, sh.opt.filter.as_ref()) {
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

/// 单个 op 的结果入账：错误绝不中断整体（FFS 累积语义），
/// 逐条经 sink 发 Error 事件（windowed 桌面构建首次真正看得见错误）＋保留 eprintln 给 CLI。
fn record(sh: &Shared, op: &Op, res: std::io::Result<()>, pp: &PhaseProgress, acc: &Counters) {
    let label = format!(
        "[{}] {:?} {}",
        if op.side == Side::Target { "target" } else { "source" },
        op.action,
        op.path
    );
    match res {
        Ok(_) => {
            acc.done.fetch_add(1, Ordering::Relaxed);
            if sh.opt.verbose {
                println!("OK   {label}");
            }
            pp.item_done(&op.path);
        }
        Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
            // 目录留着不是错误（保护过滤中的文件是对的），但必须让人看见
            acc.skipped.fetch_add(1, Ordering::Relaxed);
            println!("KEPT      {} ({e})", op.path);
            pp.item_done(&op.path);
        }
        Err(e) if crate::progress::is_cancelled(&e) && sh.ctx.ctl.cancelled() => {
            // 用户取消：这个 op 没完成也不算错——摘要里 cancelled=true 说明一切
        }
        Err(e) => {
            acc.errors.fetch_add(1, Ordering::Relaxed);
            eprintln!("ERR  {label}: {e}");
            pp.error(
                &op.path,
                &format!("{:?}", op.action),
                if op.side == Side::Target { "target" } else { "source" },
                &e.to_string(),
            );
            pp.item_done(&op.path);
        }
    }
}

/// 跑一类 op。width==1 在当前线程顺序执行；否则 scoped 线程池
/// （AtomicUsize 工单索引，不切分区间——大文件不会把某个 worker 拖成长尾）。
/// 不用 rayon：verify 的 blake3 已在 rayon 全局池里，嵌套会过订阅且缠住暂停语义。
fn run_class(class: &[&Op], width: usize, sh: &Shared, pp: &PhaseProgress, acc: &Counters) {
    if class.is_empty() {
        return;
    }
    let width = width.clamp(1, 16).min(class.len());
    let next = AtomicUsize::new(0);
    let work = || {
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= class.len() {
                break;
            }
            let op = class[i];
            // 相邻两个 op 之间的协作点（暂停在这里打转、取消在这里退场）
            if pp.checkpoint().is_err() {
                break;
            }
            let res = exec_op(sh, op, pp);
            let bail = matches!(&res, Err(e) if crate::progress::is_cancelled(e) && sh.ctx.ctl.cancelled());
            record(sh, op, res, pp, acc);
            if bail {
                break;
            }
        }
    };
    if width == 1 {
        work();
    } else {
        std::thread::scope(|s| {
            for _ in 0..width {
                s.spawn(&work);
            }
        });
    }
}

pub fn apply(ops: &[Op], source_root: &Path, target_root: &Path, opt: &ApplyOptions) -> (u64, u64, u64) {
    apply_with(ops, source_root, target_root, opt, &RunCtx::null()).into_tuple()
}

/// v0.9 M1：带进度/取消/暂停的执行主体。五个串行相位：
/// Moves → **Copy/Update（并行）** → Chmod → Delete → DeleteDir（类内 deepest-first）。
/// 开 delta 的 Update 留在串行道——update_with_delta 会把新旧两份读进内存（≤1GB 帽），
/// 并行 4 工 × 2GB 峰值不可接受。
pub fn apply_with(
    ops: &[Op],
    source_root: &Path,
    target_root: &Path,
    opt: &ApplyOptions,
    ctx: &RunCtx,
) -> ApplyOutcome {
    // Apply 的总量开跑前即知：UI 百分比公式从 t=0 生效
    let items_total = ops.iter().filter(|o| !matches!(o.action, Action::Conflict | Action::Note)).count() as u64;
    let bytes_total: u64 = ops
        .iter()
        .filter(|o| matches!(o.action, Action::Copy | Action::Update) && o.link.is_none())
        .filter_map(|o| o.size)
        .sum();
    let pp = PhaseProgress::begin(ctx, Phase::Apply, None, items_total, bytes_total);
    let acc = Counters::default();

    // Conflict/Note 只报告，与执行相位无关，先走完
    for op in ops {
        match op.action {
            Action::Conflict => {
                println!("CONFLICT  {} ({})", op.path, op.reason);
                acc.skipped.fetch_add(1, Ordering::Relaxed);
            }
            Action::Note => {
                println!("NOTE      {} ({} from={})", op.path, op.reason, op.from.clone().unwrap_or_default());
                acc.skipped.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    if opt.dry_run {
        for op in ops.iter().filter(|o| !matches!(o.action, Action::Conflict | Action::Note)) {
            if opt.verbose {
                let label = format!(
                    "[{}] {:?} {}",
                    if op.side == Side::Target { "target" } else { "source" },
                    op.action,
                    op.path
                );
                println!("DRY  {label}  ({})", op.reason);
            }
            acc.skipped.fetch_add(1, Ordering::Relaxed);
        }
        return ApplyOutcome {
            done: 0,
            skipped: acc.skipped.load(Ordering::Relaxed),
            errors: 0,
            bytes_copied: 0,
            cancelled: false,
        };
    }

    // FFS dir_lock 思路：动手前锁两侧 root（带心跳），防两台机器同时 apply 同一目录。
    // 暂停用 100ms 自旋而非挂起返回，正是为了让这两个锁的心跳线程在暂停期间继续跳。
    let _lock_guard: (crate::lock::RootLock, crate::lock::RootLock) = {
        let ls = match crate::lock::RootLock::acquire(source_root) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("cannot lock source root: {e}");
                return ApplyOutcome { done: 0, skipped: ops.len() as u64, errors: 1, bytes_copied: 0, cancelled: false };
            }
        };
        let lt = match crate::lock::RootLock::acquire(target_root) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("cannot lock target root: {e}");
                return ApplyOutcome { done: 0, skipped: ops.len() as u64, errors: 1, bytes_copied: 0, cancelled: false };
            }
        };
        (ls, lt)
    };

    let sh = Shared {
        opt,
        ctx,
        source_root,
        target_root,
        trash: opt.trash.clone().unwrap_or_else(default_trash),
        ver_source: Mutex::new(None),
        ver_target: Mutex::new(None),
        mtime_fixes: Mutex::new(Vec::new()),
        delta_saved: AtomicU64::new(0),
    };

    // 分相（不信输入顺序）
    let mut moves: Vec<&Op> = Vec::new();
    let mut copies: Vec<&Op> = Vec::new();
    let mut copies_delta: Vec<&Op> = Vec::new();
    let mut chmods: Vec<&Op> = Vec::new();
    let mut deletes: Vec<&Op> = Vec::new();
    let mut deldirs: Vec<&Op> = Vec::new();
    for op in ops {
        match op.action {
            Action::Move => moves.push(op),
            Action::Copy | Action::Update => {
                if opt.delta && op.action == Action::Update && op.link.is_none() {
                    copies_delta.push(op);
                } else {
                    copies.push(op);
                }
            }
            Action::Chmod => chmods.push(op),
            Action::Delete => deletes.push(op),
            Action::DeleteDir => deldirs.push(op),
            Action::Conflict | Action::Note => {}
        }
    }
    // 深目录先删，父目录才可能变空
    deldirs.sort_by_key(|o| std::cmp::Reverse(o.path.matches('/').count()));

    // 同一 (side, path) 出现两次（理论上计划不会生成，但翻向操作人为可造）→ 并行会竞写，强制串行
    let mut seen = std::collections::HashSet::new();
    let has_dup = copies.iter().any(|o| !seen.insert((o.side == Side::Source, o.path.as_str())));
    let width = if has_dup { 1 } else { opt.parallel };

    run_class(&moves, 1, &sh, &pp, &acc);
    run_class(&copies, width, &sh, &pp, &acc);
    run_class(&copies_delta, 1, &sh, &pp, &acc);
    run_class(&chmods, 1, &sh, &pp, &acc);
    run_class(&deletes, 1, &sh, &pp, &acc);
    run_class(&deldirs, 1, &sh, &pp, &acc);

    // ---- 串行收尾 ----
    let mtime_fixes = std::mem::take(&mut *sh.mtime_fixes.lock().unwrap());
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
    let delta_saved = sh.delta_saved.load(Ordering::Relaxed);
    if delta_saved > 0 {
        println!("delta: {} not re-written", crate::preflight::human_bytes(delta_saved));
    }
    if let Some(w) = sh.ver_source.into_inner().unwrap() {
        let side_ops: Vec<Op> = ops.iter().filter(|o| o.side == Side::Source).cloned().collect();
        if let Ok(Some(id)) = w.finish(&side_ops) {
            println!("version saved: {} (id {id})", source_root.join(crate::version::STORE_DIR).display());
        }
    }
    if let Some(w) = sh.ver_target.into_inner().unwrap() {
        let side_ops: Vec<Op> = ops.iter().filter(|o| o.side == Side::Target).cloned().collect();
        if let Ok(Some(id)) = w.finish(&side_ops) {
            println!("version saved: {} (id {id})", target_root.join(crate::version::STORE_DIR).display());
        }
    }
    let done = acc.done.load(Ordering::Relaxed);
    if !opt.versioning && done > 0 {
        println!("trash (deleted/overwritten files kept at): {}", sh.trash.display());
    }
    ApplyOutcome {
        done,
        skipped: acc.skipped.load(Ordering::Relaxed),
        errors: acc.errors.load(Ordering::Relaxed),
        bytes_copied: pp.counts().2,
        cancelled: ctx.ctl.cancelled(),
    }
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

    // ---- v0.9 M1：并行/进度/取消 ----

    use crate::progress::{ProgressEvent, RunCtl, RunCtx};
    use std::sync::Arc;

    fn collecting_ctx() -> (RunCtx, Arc<Mutex<Vec<ProgressEvent>>>) {
        let store: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let s2 = store.clone();
        let sink = move |ev: ProgressEvent| {
            s2.lock().unwrap().push(ev);
        };
        (RunCtx::new(RunCtl::new(), Arc::new(sink)), store)
    }

    /// 并行与串行必须产出逐字节相同的结果树（含 trash 保存集）
    #[test]
    fn parallel_and_sequential_agree() {
        let run = |tag: &str, parallel: usize| -> (PathBuf, PathBuf, (u64, u64, u64)) {
            let base = tmproot(&format!("par-{tag}"));
            let (s, t) = (base.join("s"), base.join("t"));
            std::fs::create_dir_all(&s).unwrap();
            std::fs::create_dir_all(&t).unwrap();
            let mut ops = Vec::new();
            for i in 0..12 {
                let name = format!("f{i:02}.bin");
                std::fs::write(s.join(&name), vec![i as u8; 20_000 + i * 1000]).unwrap();
                ops.push(op(Action::Copy, &name));
            }
            // 混入一个 update（旧内容进 trash）与一个 delete
            std::fs::write(s.join("up.txt"), b"NEW").unwrap();
            std::fs::write(t.join("up.txt"), b"OLD").unwrap();
            ops.push(op(Action::Update, "up.txt"));
            std::fs::write(t.join("gone.txt"), b"bye").unwrap();
            ops.push(op(Action::Delete, "gone.txt"));

            let mut o = opts(base.join("trash"));
            o.parallel = parallel;
            let r = apply(&ops, &s, &t, &o);
            (base.clone(), t, r)
        };
        let (b1, t1, r1) = run("seq", 1);
        let (b2, t2, r2) = run("par", 4);
        assert_eq!(r1, r2, "counts must match");
        // 结果树逐文件一致
        for e in std::fs::read_dir(&t1).unwrap().flatten() {
            let name = e.file_name();
            let a = std::fs::read(e.path()).unwrap();
            let b = std::fs::read(t2.join(&name)).unwrap();
            assert_eq!(a, b, "file {:?} differs between seq and par runs", name);
        }
        assert_eq!(
            std::fs::read_dir(&t1).unwrap().count(),
            std::fs::read_dir(&t2).unwrap().count()
        );
        let _ = std::fs::remove_dir_all(&b1);
        let _ = std::fs::remove_dir_all(&b2);
    }

    /// 错误不中断：10 好 1 坏 → 10 应用 + 1 Error 事件，字节账目 = Σ 成功文件
    #[test]
    fn errors_accumulate_without_aborting() {
        let base = tmproot("errs");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        let mut ops = Vec::new();
        let mut expect_bytes = 0u64;
        for i in 0..10 {
            let name = format!("g{i}.bin");
            let body = vec![9u8; 5_000];
            std::fs::write(s.join(&name), &body).unwrap();
            expect_bytes += body.len() as u64;
            let mut o = op(Action::Copy, &name);
            o.size = Some(body.len() as u64);
            ops.push(o);
        }
        ops.push(op(Action::Copy, "missing-on-source.bin")); // 必失败

        let (ctx, store) = collecting_ctx();
        let out = apply_with(&ops, &s, &t, &opts(base.join("trash")), &ctx);
        assert_eq!(out.done, 10);
        assert_eq!(out.errors, 1);
        assert!(!out.cancelled);
        assert_eq!(out.bytes_copied, expect_bytes, "byte ledger must equal the sum of successful copies");
        let evs = store.lock().unwrap();
        assert_eq!(
            evs.iter().filter(|e| matches!(e, ProgressEvent::Error { .. })).count(),
            1,
            "exactly one Error event"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 大文件复制中途取消：块间响应、终点无半截文件、零 .syncdash.tmp 残留
    #[test]
    fn cancel_mid_copy_leaves_no_debris() {
        let base = tmproot("cancel");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(s.join("big.bin"), vec![3u8; 64 * 1024 * 1024]).unwrap();
        let mut o = op(Action::Copy, "big.bin");
        o.size = Some(64 * 1024 * 1024);

        let ctl = RunCtl::new();
        let ctl2 = ctl.clone();
        // 第一个 Progress 事件（= 第一个 1MiB 块）后立刻请求取消
        let sink = move |ev: ProgressEvent| {
            if matches!(ev, ProgressEvent::Progress { .. }) {
                ctl2.request_cancel();
            }
        };
        let ctx = RunCtx::new(ctl, Arc::new(sink));
        let out = apply_with(&[o], &s, &t, &opts(base.join("trash")), &ctx);
        assert!(out.cancelled);
        assert_eq!(out.done, 0);
        assert_eq!(out.errors, 0, "cancellation is not an error");
        assert!(!t.join("big.bin").exists(), "no partial file may reach the destination");
        let leftovers: Vec<_> = std::fs::read_dir(&t)
            .unwrap()
            .flatten()
            .filter(|e| crate::atomic::is_temp_name(&e.file_name().to_string_lossy()))
            .collect();
        assert!(leftovers.is_empty(), "temp files must be cleaned on cancel");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// DeleteDir 深度序：子目录先于父目录被删，父目录才可能空
    #[test]
    fn delete_dirs_deepest_first_regardless_of_input_order() {
        let base = tmproot("depth");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(t.join("a").join("b").join("c")).unwrap();
        // 故意按浅→深的错误顺序给 op
        let ops = vec![op(Action::DeleteDir, "a"), op(Action::DeleteDir, "a/b"), op(Action::DeleteDir, "a/b/c")];
        let (done, _, errors) = apply(&ops, &s, &t, &opts(base.join("trash")));
        assert_eq!((done, errors), (3, 0), "all three directories must go");
        assert!(!t.join("a").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
