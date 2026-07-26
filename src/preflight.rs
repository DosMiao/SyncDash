//! 动手之前的三道闸（P0-2 / P0-3）。
//!
//! 1. **挂载点标记**——语义参照 syncthing 的 `.stfolder`
//!    （`lib/config/folderconfiguration.go:236` 的 CheckPath）。SMB 共享盘没挂上时
//!    target 往往是个空目录（甚至会被本地自动创建），此时 mirror 会生成
//!    "把 target 全删掉"或"把几十 GB 全传一遍"的计划。标记文件是唯一可靠的判据：
//!    它跟着**数据**走，盘没挂上标记就不在。
//! 2. **磁盘空间预检**——计划里每个 op 都带 size，动手前汇总一下就知道要写多少。
//!    参照 syncthing 的 `CheckAvailableSpace` / `minDiskFree`（默认 1%）。
//! 3. **计划体检**——删除占比过高时拒绝执行。syncthing 没有等价物（它是连续同步，
//!    没有"一次性大计划"这个概念），但我们的显式模型很适合这道闸：
//!    它能顺带拦住过滤器写错、source/target 写反、路径打错。

use crate::compare::{Action, Op, Side};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 领地标记文件名。与 CodeSync 生态的 `.ffs-sync` 并存，互不干扰。
pub const MARKER_NAME: &str = ".syncdash-root";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Marker {
    pub job: String,
    pub host: String,
    pub created_at_ms: u64,
    /// 自由备注，人类可读
    #[serde(default)]
    pub note: String,
}

pub fn marker_path(root: &Path) -> PathBuf {
    root.join(MARKER_NAME)
}

pub fn has_marker(root: &Path) -> bool {
    marker_path(root).is_file()
}

pub fn read_marker(root: &Path) -> Option<Marker> {
    let text = std::fs::read_to_string(marker_path(root)).ok()?;
    serde_json::from_str(&text).ok()
}

/// 写标记（`syncdash mark`）。已存在则保留原内容并报告，不覆盖。
pub fn write_marker(root: &Path, job: &str, note: &str) -> std::io::Result<(PathBuf, bool)> {
    let p = marker_path(root);
    if p.is_file() {
        return Ok((p, false));
    }
    if !root.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("not a directory: {}", root.display()),
        ));
    }
    let m = Marker {
        job: job.to_string(),
        host: crate::table::host_name(),
        created_at_ms: crate::table::now_ms(),
        note: note.to_string(),
    };
    std::fs::write(&p, format!("{}\n", serde_json::to_string_pretty(&m)?))?;
    Ok((p, true))
}

// ---------- 磁盘空间 ----------

/// 返回 (当前用户可用字节, 卷总字节)。拿不到就 None（不因此阻断，只是没法检查）。
#[cfg(windows)]
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            lp_directory_name: *const u16,
            lp_free_bytes_available_to_caller: *mut u64,
            lp_total_number_of_bytes: *mut u64,
            lp_total_number_of_free_bytes: *mut u64,
        ) -> i32;
    }
    // UNC 路径（\\host\share\...）同样受支持——正是 SMB target 需要的
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
    if ok == 0 {
        None
    } else {
        Some((avail, total))
    }
}

#[cfg(unix)]
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        // f_frsize 是"片段大小"，容量统计的正确单位；为 0 时退回 f_bsize
        let unit = if st.f_frsize > 0 { st.f_frsize as u64 } else { st.f_bsize as u64 };
        Some((st.f_bavail as u64 * unit, st.f_blocks as u64 * unit))
    }
}

pub fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

// ---------- 计划统计 ----------

#[derive(Default, Clone, Debug)]
pub struct SideStats {
    /// 需要写入的字节（copy + update）
    pub write_bytes: u64,
    pub copies: u64,
    pub updates: u64,
    pub deletes: u64,
    pub delete_dirs: u64,
    pub moves: u64,
}

#[derive(Default, Clone, Debug)]
pub struct PlanStats {
    pub source: SideStats,
    pub target: SideStats,
    pub conflicts: u64,
}

pub fn stat_plan(ops: &[Op]) -> PlanStats {
    let mut st = PlanStats::default();
    for op in ops {
        if op.action == Action::Conflict {
            st.conflicts += 1;
            continue;
        }
        let s = match op.side {
            Side::Source => &mut st.source,
            Side::Target => &mut st.target,
        };
        match op.action {
            Action::Copy => {
                s.copies += 1;
                s.write_bytes += op.size.unwrap_or(0);
            }
            Action::Update => {
                s.updates += 1;
                s.write_bytes += op.size.unwrap_or(0);
            }
            Action::Move => s.moves += 1,
            Action::Delete => s.deletes += 1,
            Action::DeleteDir => s.delete_dirs += 1,
            _ => {}
        }
    }
    st
}

// ---------- 闸门 ----------

#[derive(Clone, Debug)]
pub struct Guards {
    /// 要求两侧 root 都有 .syncdash-root 标记（防共享盘没挂上）
    pub require_marker: bool,
    /// 至少保留的空闲比例（0.01 = 1%）。<=0 关闭
    pub min_free_pct: f64,
    /// 单侧删除条目占该侧总条目的比例超过它就拒绝执行。<=0 或 >=1 关闭
    pub max_delete_ratio: f64,
    /// 用户显式放行（--i-know），只放行体检类闸门，标记/空间仍然拦
    pub acknowledged: bool,
}

impl Default for Guards {
    fn default() -> Self {
        Guards { require_marker: false, min_free_pct: 0.01, max_delete_ratio: 0.5, acknowledged: false }
    }
}

/// 一次预检的结论。`blockers` 非空 = 拒绝执行。
pub struct Verdict {
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

impl Verdict {
    pub fn ok(&self) -> bool {
        self.blockers.is_empty()
    }
    /// 把结论打印到 stderr，返回是否放行
    pub fn report(&self, tag: &str) -> bool {
        for w in &self.warnings {
            eprintln!("[{tag}] warning: {w}");
        }
        for b in &self.blockers {
            eprintln!("[{tag}] REFUSED: {b}");
        }
        self.ok()
    }
}

/// root 可用性 + 标记检查。`label` 只用于消息（"source"/"target"）。
pub fn check_root(label: &str, root: &Path, require_marker: bool, v: &mut Verdict) {
    if !root.is_dir() {
        v.blockers.push(format!("{label} root not accessible: {}", root.display()));
        return;
    }
    if require_marker && !has_marker(root) {
        v.blockers.push(format!(
            "{label} root has no {MARKER_NAME} marker: {} \
             — the share may not be mounted. Run `syncdash mark <root>` once on the real data, \
             or set require_marker = false in the job.",
            root.display()
        ));
        return;
    }
    // 即便不强制标记，空目录 + 有删除计划也值得警告（在 check_plan 里判）
    if !require_marker && !has_marker(root) {
        let empty = std::fs::read_dir(root).map(|mut d| d.next().is_none()).unwrap_or(false);
        if empty {
            v.warnings.push(format!(
                "{label} root is empty and unmarked: {} — if this share simply isn't mounted, \
                 stop now (enable require_marker to make this an error)",
                root.display()
            ));
        }
    }
}

/// 空间检查：写入侧需要 write_bytes，且写完后仍要留够 min_free_pct。
pub fn check_space(label: &str, root: &Path, need: u64, min_free_pct: f64, v: &mut Verdict) {
    if need == 0 {
        return;
    }
    let Some((avail, total)) = disk_space(root) else {
        v.warnings.push(format!("{label}: cannot determine free space on {}", root.display()));
        return;
    };
    // 10% 余量：目标可能有簇对齐/稀疏/元数据开销，且计划里的 size 是源侧的
    let need_padded = need.saturating_add(need / 10);
    let reserve = if min_free_pct > 0.0 { (total as f64 * min_free_pct) as u64 } else { 0 };
    if avail < need_padded.saturating_add(reserve) {
        v.blockers.push(format!(
            "{label}: insufficient space on {} — need {} (+10% margin) and want {} free afterwards, but only {} available",
            root.display(),
            human_bytes(need),
            human_bytes(reserve),
            human_bytes(avail),
        ));
    }
}

/// 计划体检：删除占比。`entries` 是该侧快照的条目数（0 = 无从判断，跳过）。
pub fn check_delete_ratio(
    label: &str,
    side: &SideStats,
    entries: u64,
    g: &Guards,
    v: &mut Verdict,
) {
    let removals = side.deletes;
    if removals == 0 || entries == 0 {
        return;
    }
    if !(g.max_delete_ratio > 0.0 && g.max_delete_ratio < 1.0) {
        return;
    }
    let ratio = removals as f64 / entries as f64;
    if ratio < g.max_delete_ratio {
        return;
    }
    let msg = format!(
        "{label}: plan deletes {removals} of {entries} entries ({:.0}%) — over the {:.0}% guard. \
         A wrong filter, an unmounted share, or swapped source/target all look exactly like this.",
        ratio * 100.0,
        g.max_delete_ratio * 100.0
    );
    if g.acknowledged {
        v.warnings.push(format!("{msg} (allowed by --i-know)"));
    } else {
        v.blockers.push(format!("{msg} Re-run with --i-know if this is really intended."));
    }
}

/// 一次性跑完全部闸门。`source_entries` / `target_entries` 来自两侧快照。
#[allow(clippy::too_many_arguments)]
pub fn run_all(
    ops: &[Op],
    source_root: &Path,
    target_root: &Path,
    source_entries: u64,
    target_entries: u64,
    g: &Guards,
) -> Verdict {
    let mut v = Verdict { blockers: Vec::new(), warnings: Vec::new() };
    check_root("source", source_root, g.require_marker, &mut v);
    check_root("target", target_root, g.require_marker, &mut v);
    if !v.ok() {
        return v; // root 不可用时后面的检查没意义
    }
    let st = stat_plan(ops);
    check_space("target", target_root, st.target.write_bytes, g.min_free_pct, &mut v);
    check_space("source", source_root, st.source.write_bytes, g.min_free_pct, &mut v);
    check_delete_ratio("target", &st.target, target_entries, g, &mut v);
    check_delete_ratio("source", &st.source, source_entries, g, &mut v);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::{Action, Op, Side};

    fn op(side: Side, action: Action, path: &str, size: Option<u64>) -> Op {
        Op {
            side,
            action,
            path: path.into(),
            from: None,
            size,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "t".into(),
        }
    }

    #[test]
    fn stats_split_by_side() {
        let ops = vec![
            op(Side::Target, Action::Copy, "a", Some(100)),
            op(Side::Target, Action::Update, "b", Some(50)),
            op(Side::Target, Action::Delete, "c", Some(7)),
            op(Side::Source, Action::Copy, "d", Some(9)),
            op(Side::Target, Action::Conflict, "e", None),
        ];
        let st = stat_plan(&ops);
        assert_eq!(st.target.write_bytes, 150);
        assert_eq!(st.target.deletes, 1);
        assert_eq!(st.source.write_bytes, 9);
        assert_eq!(st.conflicts, 1);
    }

    #[test]
    fn delete_ratio_blocks_and_can_be_acknowledged() {
        let side = SideStats { deletes: 60, ..Default::default() };
        let g = Guards::default();
        let mut v = Verdict { blockers: vec![], warnings: vec![] };
        check_delete_ratio("target", &side, 100, &g, &mut v);
        assert_eq!(v.blockers.len(), 1, "60% deletion must be blocked");

        let g2 = Guards { acknowledged: true, ..Guards::default() };
        let mut v2 = Verdict { blockers: vec![], warnings: vec![] };
        check_delete_ratio("target", &side, 100, &g2, &mut v2);
        assert!(v2.ok(), "--i-know must let it through");
        assert_eq!(v2.warnings.len(), 1, "but it must still be reported");
    }

    #[test]
    fn small_deletions_pass() {
        let side = SideStats { deletes: 3, ..Default::default() };
        let mut v = Verdict { blockers: vec![], warnings: vec![] };
        check_delete_ratio("target", &side, 1000, &Guards::default(), &mut v);
        assert!(v.ok());
    }

    #[test]
    fn missing_marker_blocks_when_required() {
        let d = std::env::temp_dir().join(format!("syncdash-pf-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let mut v = Verdict { blockers: vec![], warnings: vec![] };
        check_root("target", &d, true, &mut v);
        assert_eq!(v.blockers.len(), 1);

        write_marker(&d, "test-job", "").unwrap();
        let mut v2 = Verdict { blockers: vec![], warnings: vec![] };
        check_root("target", &d, true, &mut v2);
        assert!(v2.ok(), "marker present -> pass");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn disk_space_reports_something_for_temp_dir() {
        // 不断言具体数值，只断言在本机拿得到——拿不到会退化成 warning 而非阻断
        let got = disk_space(&std::env::temp_dir());
        assert!(got.is_some(), "free space query should work on the temp volume");
        let (avail, total) = got.unwrap();
        assert!(total > 0 && avail <= total);
    }
}
