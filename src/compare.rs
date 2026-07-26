//! compare：N 表比对 → 行动计划（要求 2/3）。
//! 模式语义：
//!   mirror  —— source 为 master：target 补齐/更新/删除，完全向 source 看齐；
//!              移动检测：source 独有路径与 target 独有路径按 (hash,size) 配对 → 生成 move（治 FFS 删+增）
//!   enrich  —— 只增不删：target 缺的补上，source 较新的更新过去；不删除、不移动、不回退
//!   sync    —— 双向。带 --archive（上次同步存档，Unison 思路）时可区分"删除 vs 新增"并
//!              归因移动；无 archive 时退化为安全模式：双向补齐 + 差异报冲突 + 疑似移动只报告
//!
//! 跨平台严谨性：
//!   - 比对键 = NFC 归一化（APFS/HFS+ 会给出 NFD，Windows/Linux 惯例 NFC）＋大小写折叠
//!     （NTFS/APFS 默认都大小写不敏感）；落盘 I/O 用各侧自己的原拼写，绝不改写对方的形态
//!   - 一侧内部归一化后撞名（NFD/NFC 双胞胎、大小写双胞胎）→ Note 报告，保留先出现者
//!   - 要在 Windows 侧新建的路径先做合法性预检（保留名/非法字符/尾点尾空格）→ 不合法直接标
//!     Conflict("illegal-on-windows")，绝不执行到一半才炸
//!   - 相等判定：双方都有 hash → 按 hash；否则 size 相等且 |Δmtime| <= 2s（FAT/SMB 时间粒度）

use crate::table::{now_ms, Entry, EntryKind, Snapshot};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use unicode_normalization::UnicodeNormalization;

pub const MTIME_SLACK_MS: i64 = 2000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Copy,
    Update,
    Move,
    Delete,
    DeleteDir,
    /// 只有权限位不同：不重传内容，只回写 mode
    /// （syncthing 的 shortcutFile，`lib/model/folder_sendrecv.go:1253`）
    Chmod,
    Conflict,
    Note,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Source,
    Target,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Op {
    pub side: Side,
    pub action: Action,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mtime_ms: Option<i64>,
    /// 复制/更新内容的期望 hash（paranoid 模式复制后校验用）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hash: Option<String>,
    /// Some = 这是一个 symlink 操作，值为链接指向（apply 创建链接而非复制内容）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub link: Option<String>,
    /// 目标 unix 权限位。Chmod 用它作为要写入的值；Copy/Update 带上它则复制后一并回写
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mode: Option<u32>,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PlanHeader {
    pub schema: u32,
    pub kind: String,
    pub mode: String,
    pub generated_at_ms: u64,
    pub source_root: String,
    pub source_host: String,
    pub target_root: String,
    pub target_host: String,
    pub op_count: u64,
    pub conflict_count: u64,
    /// 两侧快照的条目数。计划体检（删除占比）要用，也让计划文件本身可自证规模。
    #[serde(default)]
    pub source_entries: u64,
    #[serde(default)]
    pub target_entries: u64,
}

pub struct Plan {
    pub header: PlanHeader,
    pub ops: Vec<Op>,
}

impl Plan {
    pub fn write_to(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "{}", serde_json::to_string(&self.header)?)?;
        for op in &self.ops {
            writeln!(w, "{}", serde_json::to_string(op)?)?;
        }
        Ok(())
    }
    pub fn load(path: &std::path::Path) -> std::io::Result<Plan> {
        use std::io::BufRead;
        let f = std::fs::File::open(path)?;
        let mut lines = std::io::BufReader::new(f).lines();
        let head = lines.next().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "empty plan"))??;
        let header: PlanHeader = serde_json::from_str(&head)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad plan header: {e}")))?;
        let mut ops = Vec::new();
        for line in lines {
            let line = line?;
            if line.trim().is_empty() { continue; }
            ops.push(serde_json::from_str(&line)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad op: {e}")))?);
        }
        Ok(Plan { header, ops })
    }
}

/// 冲突处理策略。默认 Report（只报告，绝不自动仲裁）——这是 SyncDash 的立身之本，
/// 不因为对齐 syncthing 而改默认。
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ConflictPolicy {
    /// 只报告，人来处理
    Report,
    /// 败方改名成 `<name>.sync-conflict-<ts>-<host><ext>`，胜方落地
    /// （syncthing `conflictName`，`lib/model/folder_sendrecv.go:2219`）
    Copy,
    /// mtime 新者胜，旧的直接被覆盖（不留副本）
    Newer,
}

#[derive(Clone, Copy)]
pub struct CompareOptions {
    /// 默认 true：NTFS 与 APFS 默认都大小写不敏感
    pub case_insensitive: bool,
    /// 冲突策略
    pub conflict: ConflictPolicy,
    /// 同步 unix 权限位（两侧都是 unix 时才有意义；Win 侧无 mode，开了会一直报差异）
    pub sync_mode: bool,
    /// 每个文件最多保留几份冲突副本（-1 = 不限）。仅 ConflictPolicy::Copy 有效
    pub max_conflicts: i32,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions {
            case_insensitive: true,
            conflict: ConflictPolicy::Report,
            sync_mode: false,
            max_conflicts: 5,
        }
    }
}

/// 冲突副本名：`report.pdf` → `report.sync-conflict-20260726-143000-WIN01.pdf`
/// （与 syncthing 的命名同构，便于人一眼认出，也便于双方的过滤器识别）
pub fn conflict_name(path: &str, host: &str, at_ms: u64) -> String {
    let (dir, base) = match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    // 只认最后一个点之后的扩展名；隐藏文件（.gitignore）不当作扩展名
    let (stem, ext) = match base.rfind('.') {
        Some(i) if i > 0 => (&base[..i], &base[i..]),
        _ => (base, ""),
    };
    let safe_host: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    format!("{dir}{stem}.sync-conflict-{}-{safe_host}{ext}", stamp(at_ms))
}

/// 冲突副本自身不该再参与同步/冲突判定（syncthing `isConflict`，:2224）
pub fn is_conflict_copy(path: &str) -> bool {
    path.rsplit('/').next().unwrap_or(path).contains(".sync-conflict-")
}

/// UTC 时间戳 YYYYMMDD-HHMMSS（不引入 chrono：只需要一个稳定可读的标签）
fn stamp(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    // 民用历法换算（Howard Hinnant 的 civil_from_days）
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", y, m, d, tod / 3600, (tod % 3600) / 60, tod % 60)
}

/// 比对键：NFC 归一化 ＋（可选）大小写折叠。只用于匹配，不用于 I/O。
fn norm_key(p: &str, ci: bool) -> String {
    let nfc: String = p.nfc().collect();
    if ci { nfc.to_uppercase() } else { nfc }
}

fn files_equal(a: &Entry, b: &Entry) -> bool {
    if let (Some(ha), Some(hb)) = (&a.hash, &b.hash) {
        return ha == hb;
    }
    a.size == b.size && (a.mtime_ms - b.mtime_ms).abs() <= MTIME_SLACK_MS
}

/// `e` 的内容对应 archive 条目 `r` 的第几代：
/// `Some(0)` = 与 archive 当前记录一致，`Some(n)` = 第 n 代历史，`None` = archive 没见过。
/// 代数越小越新——这让"落后一代"与"并发修改"能被区分开（P1-3）。
fn generation_of(e: &Entry, r: &Entry) -> Option<usize> {
    if files_equal(e, r) {
        return Some(0);
    }
    let h = e.hash.as_deref()?;
    r.prev.as_ref()?.iter().position(|x| x == h).map(|i| i + 1)
}

/// 归一键 → 条目；撞名（NFD/NFC 或大小写双胞胎）保留先出现者并记录
fn map_of<'a>(snap: &'a Snapshot, kind: EntryKind, ci: bool) -> (BTreeMap<String, &'a Entry>, Vec<String>) {
    let mut m: BTreeMap<String, &Entry> = BTreeMap::new();
    let mut dups = Vec::new();
    for e in snap.entries.iter().filter(|e| e.kind == kind) {
        let k = norm_key(&e.path, ci);
        if m.contains_key(&k) {
            dups.push(e.path.clone());
        } else {
            m.insert(k, e);
        }
    }
    (m, dups)
}

// ---------- 证据层（只读，界面用；compare() 不受影响） ----------

/// 比对时点某一侧的实测状态。**只供展示与排序**，apply 一个字节都不读它。
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct SideMeta {
    pub size: u64,
    pub mtime_ms: i64,
}

/// 与 `plan.ops[i]` 一一对应的两侧实测状态（缺席的一侧为 None）
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct RowMeta {
    pub src: Option<SideMeta>,
    pub dst: Option<SideMeta>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct Evidence {
    /// 长度恒等于 plan.ops.len()
    pub metas: Vec<RowMeta>,
    /// 两侧都有且判定相等的文件数 —— FFS 那句 "Showing 481 of 23,112" 的分母来源
    pub equal_count: u64,
    pub equal_bytes: u64,
}

/// 计划之外的"证据"：每行两侧的实测 size/mtime + 相等项统计。
///
/// 为什么不把这两个字段直接塞进 `Op`：`Op { .. }` 的结构体字面量在本文件里有
/// 三十多处，加字段等于改三十多个点，还会改变 plan JSONL 的落盘格式与 CLI 行为。
/// 这里走 `PlanDto.reversed` 同款的平行数组，`compare()` 一行不动。
///
/// 口径与 `compare()` 共用 `norm_key` / `files_equal`，不可能漂移。
pub fn evidence(source: &Snapshot, target: &Snapshot, plan: &Plan, copts: &CompareOptions) -> Evidence {
    let ci = copts.case_insensitive;
    let (s_files, _) = map_of(source, EntryKind::File, ci);
    let (t_files, _) = map_of(target, EntryKind::File, ci);
    let (s_dirs, _) = map_of(source, EntryKind::Dir, ci);
    let (t_dirs, _) = map_of(target, EntryKind::Dir, ci);

    let meta = |e: &Entry| SideMeta { size: e.size, mtime_ms: e.mtime_ms };
    let look = |files: &BTreeMap<String, &Entry>, dirs: &BTreeMap<String, &Entry>, rel: &str| -> Option<SideMeta> {
        let k = norm_key(rel, ci);
        files.get(&k).or_else(|| dirs.get(&k)).map(|e| meta(e))
    };

    let metas = plan
        .ops
        .iter()
        .map(|op| {
            // move 的执行侧此刻还叫 from，对面已经是 path——两侧各查各的名字
            let (s_rel, t_rel) = match (&op.action, &op.side) {
                (Action::Move, Side::Target) => (op.path.as_str(), op.from.as_deref().unwrap_or(&op.path)),
                (Action::Move, Side::Source) => (op.from.as_deref().unwrap_or(&op.path), op.path.as_str()),
                _ => (op.path.as_str(), op.path.as_str()),
            };
            RowMeta {
                src: look(&s_files, &s_dirs, s_rel),
                dst: look(&t_files, &t_dirs, t_rel),
            }
        })
        .collect();

    let mut equal_count = 0u64;
    let mut equal_bytes = 0u64;
    for (k, se) in &s_files {
        if let Some(te) = t_files.get(k) {
            if files_equal(se, te) {
                equal_count += 1;
                equal_bytes += se.size;
            }
        }
    }
    Evidence { metas, equal_count, equal_bytes }
}

/// 一条"两侧相同"的记录。计划里没有它——它不是动作，是证据。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SameRow {
    pub path: String,
    pub size: u64,
    pub mtime_ms: i64,
    /// target 侧的时间（内容相同但时间戳可以差几毫秒——FAT/SMB 粒度）
    pub other_mtime_ms: i64,
}

/// 两侧判定相等的文件，按 source 侧路径序分页。
/// FFS 底部那个 "22,631" 按钮的数据源：一个文件没出现在差异表里时，
/// 你得能确认它是"相等"，而不是"根本没扫到"。
pub fn same_page(
    source: &Snapshot,
    target: &Snapshot,
    copts: &CompareOptions,
    query: &str,
    offset: usize,
    limit: usize,
) -> (u64, Vec<SameRow>) {
    let ci = copts.case_insensitive;
    let (s_files, _) = map_of(source, EntryKind::File, ci);
    let (t_files, _) = map_of(target, EntryKind::File, ci);
    let q = query.trim().to_lowercase();
    let mut total = 0u64;
    let mut out = Vec::new();
    for (k, se) in &s_files {
        let Some(te) = t_files.get(k) else { continue };
        if !files_equal(se, te) {
            continue;
        }
        if !q.is_empty() && !se.path.to_lowercase().contains(&q) {
            continue;
        }
        total += 1;
        let idx = (total - 1) as usize;
        if idx >= offset && out.len() < limit {
            out.push(SameRow {
                path: se.path.clone(),
                size: se.size,
                mtime_ms: se.mtime_ms,
                other_mtime_ms: te.mtime_ms,
            });
        }
    }
    (total, out)
}

/// Windows 侧新建此相对路径是否合法（保留名 / 非法字符 / 尾点尾空格）
fn win_invalid_reason(rel: &str) -> Option<String> {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL",
        "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    for seg in rel.split('/') {
        if seg.is_empty() {
            continue;
        }
        if seg.ends_with('.') || seg.ends_with(' ') {
            return Some(format!("'{seg}' ends with dot/space"));
        }
        let base = seg.split('.').next().unwrap_or("").to_ascii_uppercase();
        if RESERVED.contains(&base.as_str()) {
            return Some(format!("reserved device name '{seg}'"));
        }
        if let Some(c) = seg.chars().find(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\') || (*c as u32) < 0x20) {
            return Some(format!("invalid character {c:?} in '{seg}'"));
        }
    }
    None
}

/// 一次移动配对的结果
pub struct MovePair {
    pub from: String,
    pub to: String,
    pub size: u64,
    /// 同父目录 = 就地改名（FFS "同目录 rename 合并"）
    pub rename_in_place: bool,
    /// 配对时的同内容候选个数。>1 说明 `from` 是从多个等价候选里任选的——
    /// 结果内容仍然正确，但归因不确定，reason 里必须如实标注，不能假装确定。
    pub candidates: usize,
}

/// 移动配对。配对优先级（借 FFS "同目录 rename 合并"之意）：
///   1) 同父目录（就地改名）  2) 同文件名（整目录搬迁）  3) 任意同 hash
///
/// **空文件不参与配对**：所有零长文件的 blake3 相同，会挤进同一个桶，
/// 把一堆互不相干的 `__init__.py` / `.gitkeep` 配成"重命名"。
/// syncthing 在 `findRename`（`lib/model/folder.go:930-932`）第一件事就是排除
/// `Size == 0`，我们照做。
fn detect_moves<'a>(
    adds: Vec<&'a Entry>,
    dels: Vec<&'a Entry>,
) -> (Vec<MovePair>, Vec<&'a Entry>, Vec<&'a Entry>) {
    fn parent(p: &str) -> &str {
        p.rfind('/').map(|i| &p[..i]).unwrap_or("")
    }
    let eligible = |e: &Entry| e.size > 0 && e.hash.is_some() && !is_conflict_copy(&e.path);
    let mut by_key: HashMap<(String, u64), Vec<&'a Entry>> = HashMap::new();
    for &d in &dels {
        if eligible(d) {
            by_key.entry((d.hash.clone().unwrap(), d.size)).or_default().push(d);
        }
    }
    let mut moves = Vec::new();
    let mut rest_adds = Vec::new();
    let mut used: HashSet<String> = HashSet::new();
    for a in adds {
        let mut matched = None;
        if eligible(a) {
            if let Some(cands) = by_key.get_mut(&(a.hash.clone().unwrap(), a.size)) {
                if !cands.is_empty() {
                    let n = cands.len();
                    let pick = cands
                        .iter()
                        .position(|c| parent(&c.path) == parent(&a.path))
                        .or_else(|| {
                            cands.iter().position(|c| {
                                std::path::Path::new(&c.path).file_name()
                                    == std::path::Path::new(&a.path).file_name()
                            })
                        })
                        .unwrap_or(0);
                    let c = cands.remove(pick);
                    used.insert(c.path.clone());
                    matched = Some(MovePair {
                        rename_in_place: parent(&c.path) == parent(&a.path),
                        from: c.path.clone(),
                        to: a.path.clone(),
                        size: a.size,
                        candidates: n,
                    });
                }
            }
        }
        match matched {
            Some(m) => moves.push(m),
            None => rest_adds.push(a),
        }
    }
    let rest_dels = dels.into_iter().filter(|d| !used.contains(&d.path)).collect();
    (moves, rest_adds, rest_dels)
}

/// move op 的 reason：歧义配对如实标注候选数
fn move_reason(base: &str, m: &MovePair) -> String {
    if m.candidates > 1 {
        format!("{base} (ambiguous: {} identical candidates)", m.candidates)
    } else {
        base.to_string()
    }
}

/// GUI 逐行翻方向（FFS 同款交互的语义核心）。返回 None = 该 op 不可反转（move/dir/conflict/note）。
/// - Copy 的反向：不把文件补过去，而是删掉"多出"的那份（让持有侧向缺失侧看齐）
/// - Update 的反向：让另一侧的内容获胜
/// - Delete 的反向：不删，反而把它复制回对面（恢复）
pub fn reverse_op(op: &Op) -> Option<Op> {
    let other = match op.side {
        Side::Source => Side::Target,
        Side::Target => Side::Source,
    };
    match op.action {
        Action::Copy => Some(Op {
            side: other,
            action: Action::Delete,
            path: op.path.clone(),
            from: None,
            size: op.size,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: format!("flipped({})", op.reason),
        }),
        Action::Update => Some(Op {
            side: other,
            action: Action::Update,
            path: op.path.clone(),
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: format!("flipped({})", op.reason),
        }),
        Action::Delete => Some(Op {
            side: other,
            action: Action::Copy,
            path: op.path.clone(),
            from: None,
            size: op.size,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: format!("flipped({})", op.reason),
        }),
        _ => None,
    }
}

fn push_copy(ops: &mut Vec<Op>, side: Side, e: &Entry, reason: &str) {
    ops.push(Op { side, action: Action::Copy, path: e.path.clone(), from: None, size: Some(e.size), mtime_ms: Some(e.mtime_ms), hash: e.hash.clone(), link: None, mode: None, reason: reason.into() });
}

pub fn compare(source: &Snapshot, target: &Snapshot, mode: &str, archive: Option<&Snapshot>, resolve_newer: bool, copts: &CompareOptions) -> Plan {
    let ci = copts.case_insensitive;
    let (s_files, s_dups) = map_of(source, EntryKind::File, ci);
    let (t_files, t_dups) = map_of(target, EntryKind::File, ci);
    let (s_dirs, _) = map_of(source, EntryKind::Dir, ci);
    let (t_dirs, _) = map_of(target, EntryKind::Dir, ci);
    let both_hashed = source.header.hashed && target.header.hashed;
    let mut ops: Vec<Op> = Vec::new();

    for d in s_dups {
        ops.push(Op { side: Side::Source, action: Action::Note, path: d, from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "duplicate-after-normalization (kept first; NFC/case twin)".into() });
    }
    for d in t_dups {
        ops.push(Op { side: Side::Target, action: Action::Note, path: d, from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "duplicate-after-normalization (kept first; NFC/case twin)".into() });
    }

    match mode {
        "mirror" | "enrich" => {
            let mut adds: Vec<&Entry> = Vec::new();
            let mut dels: Vec<&Entry> = Vec::new();
            for (p, se) in &s_files {
                let se = *se;
                match t_files.get(p) {
                    None => adds.push(se),
                    Some(&te) => {
                        if !files_equal(se, te) && (mode == "mirror" || se.mtime_ms > te.mtime_ms + MTIME_SLACK_MS) {
                            let reason = if mode == "mirror" { "differs-master-wins" } else { "source-newer" };
                            // 更新写到 target 已存在的文件上：用 target 的原拼写打开，不改对方形态
                            ops.push(Op { side: Side::Target, action: Action::Update, path: te.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: se.hash.clone(), link: None, mode: None, reason: reason.into() });
                        }
                    }
                }
            }
            if mode == "mirror" {
                for (p, te) in &t_files {
                    if !s_files.contains_key(p) {
                        dels.push(*te);
                    }
                }
                let (moves, rest_adds, rest_dels) = if both_hashed {
                    detect_moves(adds, dels)
                } else {
                    (Vec::new(), adds, dels)
                };
                for m in moves {
                    let base = if m.rename_in_place { "rename-detected-by-hash" } else { "move-detected-by-hash" };
                    let reason = move_reason(base, &m);
                    ops.push(Op { side: Side::Target, action: Action::Move, path: m.to, from: Some(m.from), size: Some(m.size), mtime_ms: None, hash: None, link: None, mode: None, reason });
                }
                for a in rest_adds { push_copy(&mut ops, Side::Target, a, "only-in-source"); }
                for d in rest_dels {
                    ops.push(Op { side: Side::Target, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, link: None, mode: None, reason: "gone-from-source".into() });
                }
                for (p, te) in &t_dirs {
                    if !s_dirs.contains_key(p) {
                        ops.push(Op { side: Side::Target, action: Action::DeleteDir, path: te.path.clone(), from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "dir-gone-from-source".into() });
                    }
                }
            } else {
                for a in adds { push_copy(&mut ops, Side::Target, a, "only-in-source"); }
            }
        }
        "sync" => {
            let (arch_files, _) = archive.map(|a| map_of(a, EntryKind::File, ci)).unwrap_or_default();
            let has_archive = archive.is_some();
            let mut s_adds: Vec<&Entry> = Vec::new();
            let mut t_adds: Vec<&Entry> = Vec::new();
            let mut del_on_target: Vec<&Entry> = Vec::new();
            let mut del_on_source: Vec<&Entry> = Vec::new();

            for (p, se) in &s_files {
                let se = *se;
                match t_files.get(p) {
                    Some(&te) => {
                        if files_equal(se, te) {
                            continue;
                        }
                        if has_archive {
                            // P1-3：不只看"是否等于 archive 当前那一代"，还看历史代。
                            // 一侧只是**落后**（停在某个旧版本）并不是并发修改——
                            // syncthing 用 PreviousBlocksHash 达到同样目的
                            // （`lib/protocol/bep_fileinfo.go:200-207`）。
                            // generation_of 返回 0 = 与 archive 当前代一致，1..n = 第 n 代历史。
                            let r = arch_files.get(p).copied();
                            let sg = r.and_then(|r| generation_of(se, r));
                            let tg = r.and_then(|r| generation_of(te, r));
                            let push_to_source = |ops: &mut Vec<Op>, why: &str| {
                                ops.push(Op { side: Side::Source, action: Action::Update, path: se.path.clone(), from: None, size: Some(te.size), mtime_ms: Some(te.mtime_ms), hash: te.hash.clone(), link: None, mode: None, reason: why.into() });
                            };
                            let push_to_target = |ops: &mut Vec<Op>, why: &str| {
                                ops.push(Op { side: Side::Target, action: Action::Update, path: te.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: se.hash.clone(), link: None, mode: None, reason: why.into() });
                            };
                            match (sg, tg) {
                                // source 停在已知版本、target 是新内容 → target 改的
                                (Some(_), None) => push_to_source(&mut ops, "target-changed"),
                                (None, Some(_)) => push_to_target(&mut ops, "source-changed"),
                                // 两侧都停在已知版本但代数不同 → 新的那一代赢，不是冲突
                                (Some(a), Some(b)) if a < b => {
                                    push_to_target(&mut ops, "target-behind-by-generations")
                                }
                                (Some(a), Some(b)) if a > b => {
                                    push_to_source(&mut ops, "source-behind-by-generations")
                                }
                                // 两侧都是 archive 从未见过的内容 → 真并发修改
                                _ => ops.push(Op { side: Side::Target, action: Action::Conflict, path: se.path.clone(), from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "both-changed".into() }),
                            }
                        } else if resolve_newer {
                            if se.mtime_ms >= te.mtime_ms {
                                ops.push(Op { side: Side::Target, action: Action::Update, path: te.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: se.hash.clone(), link: None, mode: None, reason: "differs-newer-wins".into() });
                            } else {
                                ops.push(Op { side: Side::Source, action: Action::Update, path: se.path.clone(), from: None, size: Some(te.size), mtime_ms: Some(te.mtime_ms), hash: te.hash.clone(), link: None, mode: None, reason: "differs-newer-wins".into() });
                            }
                        } else {
                            ops.push(Op { side: Side::Target, action: Action::Conflict, path: se.path.clone(), from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "differs-no-archive".into() });
                        }
                    }
                    None => {
                        if has_archive {
                            if let Some(&r) = arch_files.get(p) {
                                if files_equal(se, r) {
                                    del_on_source.push(se);
                                } else {
                                    ops.push(Op { side: Side::Target, action: Action::Conflict, path: se.path.clone(), from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "deleted-on-target-but-changed-on-source".into() });
                                }
                                continue;
                            }
                        }
                        s_adds.push(se);
                    }
                }
            }
            for (p, te) in &t_files {
                let te = *te;
                if s_files.contains_key(p) {
                    continue;
                }
                if has_archive {
                    if let Some(&r) = arch_files.get(p) {
                        if files_equal(te, r) {
                            del_on_target.push(te);
                        } else {
                            ops.push(Op { side: Side::Target, action: Action::Conflict, path: te.path.clone(), from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "deleted-on-source-but-changed-on-target".into() });
                        }
                        continue;
                    }
                }
                t_adds.push(te);
            }

            if has_archive && both_hashed {
                let (mv_on_target, rest_s_adds, rest_del_t) = detect_moves(s_adds, del_on_target);
                for m in mv_on_target {
                    let base = if m.rename_in_place { "rename-on-source-replayed" } else { "move-on-source-replayed" };
                    let reason = move_reason(base, &m);
                    ops.push(Op { side: Side::Target, action: Action::Move, path: m.to, from: Some(m.from), size: Some(m.size), mtime_ms: None, hash: None, link: None, mode: None, reason });
                }
                let (mv_on_source, rest_t_adds, rest_del_s) = detect_moves(t_adds, del_on_source);
                for m in mv_on_source {
                    let base = if m.rename_in_place { "rename-on-target-replayed" } else { "move-on-target-replayed" };
                    let reason = move_reason(base, &m);
                    ops.push(Op { side: Side::Source, action: Action::Move, path: m.to, from: Some(m.from), size: Some(m.size), mtime_ms: None, hash: None, link: None, mode: None, reason });
                }
                for a in rest_s_adds { push_copy(&mut ops, Side::Target, a, "added-on-source"); }
                for a in rest_t_adds { push_copy(&mut ops, Side::Source, a, "added-on-target"); }
                for d in rest_del_t {
                    ops.push(Op { side: Side::Target, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, link: None, mode: None, reason: "deleted-on-source".into() });
                }
                for d in rest_del_s {
                    ops.push(Op { side: Side::Source, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, link: None, mode: None, reason: "deleted-on-target".into() });
                }
            } else {
                if both_hashed {
                    let t_only: HashMap<&str, &str> = t_adds.iter()
                        .filter_map(|e| e.hash.as_deref().map(|h| (h, e.path.as_str())))
                        .collect();
                    for a in &s_adds {
                        if let Some(h) = a.hash.as_deref() {
                            if let Some(&other) = t_only.get(h) {
                                ops.push(Op { side: Side::Target, action: Action::Note, path: a.path.clone(), from: Some(other.to_string()), size: Some(a.size), mtime_ms: None, hash: None, link: None, mode: None, reason: "possible-move-needs-archive".into() });
                            }
                        }
                    }
                }
                for a in s_adds { push_copy(&mut ops, Side::Target, a, "only-in-source"); }
                for a in t_adds { push_copy(&mut ops, Side::Source, a, "only-in-target"); }
                for d in del_on_target {
                    ops.push(Op { side: Side::Target, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, link: None, mode: None, reason: "deleted-on-source".into() });
                }
                for d in del_on_source {
                    ops.push(Op { side: Side::Source, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, link: None, mode: None, reason: "deleted-on-target".into() });
                }
            }
        }
        other => panic!("unknown mode: {other}"),
    }

    // ---------- symlink（symlinks="direct" 时两侧表里才有 Symlink 条目）----------
    // 比对按"链接指向字符串"相等；mirror 向 master 看齐，enrich 只补缺，sync 只补缺＋差异报冲突
    {
        let (s_links, _) = map_of(source, EntryKind::Symlink, ci);
        let (t_links, _) = map_of(target, EntryKind::Symlink, ci);
        let link_op = |side: Side, action: Action, e: &Entry, reason: &str| Op {
            side,
            action,
            path: e.path.clone(),
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: e.link.clone(),
            mode: None,
            reason: reason.into(),
        };
        for (p, se) in &s_links {
            let se = *se;
            match t_links.get(p) {
                None => {
                    if mode == "mirror" || mode == "enrich" || mode == "sync" {
                        ops.push(link_op(Side::Target, Action::Copy, se, "symlink-only-in-source"));
                    }
                }
                Some(&te) => {
                    if se.link != te.link {
                        match mode {
                            "mirror" => ops.push(link_op(Side::Target, Action::Update, se, "symlink-differs-master-wins")),
                            "sync" => ops.push(link_op(Side::Target, Action::Conflict, se, "symlink-differs")),
                            _ => {}
                        }
                    }
                }
            }
        }
        for (p, te) in &t_links {
            let te = *te;
            if !s_links.contains_key(p) {
                match mode {
                    "mirror" => ops.push(link_op(Side::Target, Action::Delete, te, "symlink-gone-from-source")),
                    "sync" => ops.push(link_op(Side::Source, Action::Copy, te, "symlink-only-in-target")),
                    _ => {}
                }
            }
        }
    }

    // ---------- unix 权限位（P2-4）----------
    // 两侧都是 unix、且 job 显式开了 sync_mode 才做：Windows 侧没有 mode，
    // 开着会每次比对都报差异。过去 mode 只记进快照表却从不参与比对，
    // 挂载盘路径同步过去的脚本会丢 exec 位（pack 路径反而会恢复，两条路径行为不一致）。
    let both_unix = source.header.os != "windows" && target.header.os != "windows";
    if copts.sync_mode && both_unix {
        // 1) 要复制内容的 op 顺带带上目标 mode，apply 复制完直接回写，不用多一趟
        for op in &mut ops {
            if matches!(op.action, Action::Copy | Action::Update) && op.link.is_none() {
                let key = norm_key(&op.path, ci);
                let from_entry = match op.side {
                    Side::Target => s_files.get(&key),
                    Side::Source => t_files.get(&key),
                };
                if let Some(e) = from_entry {
                    op.mode = e.mode;
                }
            }
        }
        // 2) 内容相同、只有权限不同 → 单独一条 Chmod，绝不为了几个权限位重传文件
        //    （syncthing 的 shortcutFile 同样思路）。sync 模式没有"谁是 master"，
        //    权限归属无从判断，只报告不动手。
        if mode == "mirror" || mode == "enrich" {
            for (p, se) in &s_files {
                let se = *se;
                if let Some(&te) = t_files.get(p) {
                    if files_equal(se, te) && se.mode.is_some() && se.mode != te.mode {
                        ops.push(Op {
                            side: Side::Target,
                            action: Action::Chmod,
                            path: te.path.clone(),
                            from: None,
                            size: None,
                            mtime_ms: None,
                            hash: None,
                            link: None,
                            mode: se.mode,
                            reason: format!(
                                "mode-differs-master-wins ({:04o} -> {:04o})",
                                te.mode.unwrap_or(0),
                                se.mode.unwrap_or(0)
                            ),
                        });
                    }
                }
            }
        }
    }

    // ---------- 大小写敏感模式下的落盘撞名预检（P2-3）----------
    // case_sensitive = true 时比对键区分大小写，但底层 NTFS/APFS 通常**不**区分：
    // 往 target 写 `Foo.txt` 会静默覆盖已存在的 `foo.txt`。syncthing 在写入前解析
    // 目录真名并报 CaseConflictError（`lib/fs/casefs.go:27-37`）；我们在计划阶段就拦。
    if !ci {
        let fold = |p: &str| -> String { p.nfc().collect::<String>().to_uppercase() };
        let mut folded: HashMap<(bool, String), Vec<&str>> = HashMap::new();
        for (is_target, snap) in [(false, source), (true, target)] {
            for e in &snap.entries {
                folded.entry((is_target, fold(&e.path))).or_default().push(&e.path);
            }
        }
        for op in &mut ops {
            if !matches!(op.action, Action::Copy | Action::Move) {
                continue;
            }
            let is_target = op.side == Side::Target;
            if let Some(existing) = folded.get(&(is_target, fold(&op.path))) {
                // Move 的 from 就是那个"撞名"的文件时，这恰恰是一次**大小写改名**
                // （`readme.md` → `Readme.md`），是移动检测的正确产物，不是撞名事故。
                let from = op.from.as_deref();
                if let Some(other) = existing.iter().find(|p| **p != op.path && Some(**p) != from) {
                    op.action = Action::Conflict;
                    op.reason = format!(
                        "case-collision: writing '{}' would overwrite existing '{other}' on a \
                         case-insensitive filesystem (set case_sensitive = false, or rename one side)",
                        op.path
                    );
                }
            }
        }
    }

    // ---------- 冲突策略（P1-2）----------
    // 默认 Report：只报告，人来处理——这是 SyncDash 的立身之本，不改。
    // Copy/Newer 是显式 opt-in，让双机日常使用时一个冲突不会把文件卡住到天荒地老。
    // 只处理**内容冲突**（两侧都有该文件且都改过）；删改冲突与
    // illegal-on-windows 一律保持 Report——自动仲裁"删还是留"太危险。
    if copts.conflict != ConflictPolicy::Report {
        const RESOLVABLE: [&str; 3] = ["both-changed", "differs-no-archive", "symlink-differs"];
        let now = now_ms();
        let mut extra: Vec<Op> = Vec::new();
        for op in &mut ops {
            if op.action != Action::Conflict || !RESOLVABLE.contains(&op.reason.as_str()) {
                continue;
            }
            // 冲突副本自身不再生成冲突副本（syncthing isConflict，:1863）
            if is_conflict_copy(&op.path) {
                continue;
            }
            let key = norm_key(&op.path, ci);
            let (Some(&se), Some(&te)) = (s_files.get(&key), t_files.get(&key)) else { continue };
            // mtime 新者胜；完全相同则 host 名字典序做稳定 tie-break
            // （syncthing 用版本向量里的 device id，我们没有，用 host 等效）
            let source_wins = match se.mtime_ms.cmp(&te.mtime_ms) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Equal => source.header.host <= target.header.host,
            };
            let (winner, loser, loser_side, loser_host) = if source_wins {
                (se, te, Side::Target, target.header.host.as_str())
            } else {
                (te, se, Side::Source, source.header.host.as_str())
            };
            if copts.conflict == ConflictPolicy::Copy {
                let kept = conflict_name(&loser.path, loser_host, now);
                extra.push(Op {
                    side: loser_side.clone(),
                    action: Action::Move,
                    path: kept.clone(),
                    from: Some(loser.path.clone()),
                    size: Some(loser.size),
                    mtime_ms: Some(loser.mtime_ms),
                    hash: loser.hash.clone(),
                    link: None,
                    mode: None,
                    reason: format!("conflict-loser-kept-as-copy ({})", op.reason),
                });
            }
            // 胜方内容落到败方那一侧
            extra.push(Op {
                side: loser_side,
                action: Action::Update,
                path: loser.path.clone(),
                from: None,
                size: Some(winner.size),
                mtime_ms: Some(winner.mtime_ms),
                hash: winner.hash.clone(),
                link: winner.link.clone(),
                mode: None,
                reason: format!(
                    "conflict-resolved-newer-wins ({})",
                    if copts.conflict == ConflictPolicy::Copy { "loser kept as .sync-conflict copy" } else { "loser overwritten (recoverable from trash)" }
                ),
            });
            // 原冲突行降级为记录，保留可审计的痕迹
            op.action = Action::Note;
            op.reason = format!("auto-resolved: {}", op.reason);
        }
        ops.extend(extra);

        // max_conflicts：同一路径的冲突副本超额时删掉最老的几份
        // （syncthing `lib/model/folder_sendrecv.go:1888-1898`）。
        // 副本名里带时间戳，字典序即时间序。
        if copts.conflict == ConflictPolicy::Copy && copts.max_conflicts >= 0 {
            let limit = copts.max_conflicts as usize;
            for (is_target, snap) in [(false, source), (true, target)] {
                let mut groups: BTreeMap<String, Vec<&str>> = BTreeMap::new();
                for e in snap.entries.iter().filter(|e| e.kind == EntryKind::File) {
                    if is_conflict_copy(&e.path) {
                        // 归组到原始文件名：去掉 `.sync-conflict-…` 那一段
                        if let Some(i) = e.path.find(".sync-conflict-") {
                            let stem = &e.path[..i];
                            groups.entry(stem.to_string()).or_default().push(&e.path);
                        }
                    }
                }
                for (_stem, mut copies) in groups {
                    if copies.len() <= limit {
                        continue;
                    }
                    copies.sort_unstable(); // 时间戳在名字里 → 字典序 = 时间序
                    let doomed = copies.len() - limit;
                    for p in copies.into_iter().take(doomed) {
                        ops.push(Op {
                            side: if is_target { Side::Target } else { Side::Source },
                            action: Action::Delete,
                            path: p.to_string(),
                            from: None,
                            size: None,
                            mtime_ms: None,
                            hash: None,
                            link: None,
                            mode: None,
                            reason: format!("conflict-copy-over-limit (max_conflicts = {limit})"),
                        });
                    }
                }
            }
        }
    }

    // Windows 侧新建路径的合法性预检：计划阶段拦下，不让 apply 执行到一半才炸
    for op in &mut ops {
        if matches!(op.action, Action::Copy | Action::Move) {
            let exec_os = match op.side {
                Side::Target => target.header.os.as_str(),
                Side::Source => source.header.os.as_str(),
            };
            if exec_os == "windows" {
                if let Some(r) = win_invalid_reason(&op.path) {
                    op.action = Action::Conflict;
                    op.reason = format!("illegal-on-windows: {r}");
                }
            }
        }
    }

    let rank = |o: &Op| match o.action {
        Action::Move => 0,
        Action::Copy | Action::Update => 1,
        Action::Chmod => 2,
        Action::Delete => 3,
        Action::DeleteDir => 4,
        Action::Conflict | Action::Note => 5,
    };
    ops.sort_by(|a, b| {
        rank(a).cmp(&rank(b)).then_with(|| {
            if a.action == Action::DeleteDir {
                b.path.matches('/').count().cmp(&a.path.matches('/').count())
            } else {
                a.path.cmp(&b.path)
            }
        })
    });

    let conflict_count = ops.iter().filter(|o| o.action == Action::Conflict).count() as u64;
    Plan {
        header: PlanHeader {
            schema: crate::table::SCHEMA,
            kind: "plan".into(),
            mode: mode.into(),
            generated_at_ms: now_ms(),
            source_root: source.header.root.clone(),
            source_host: source.header.host.clone(),
            target_root: target.header.root.clone(),
            target_host: target.header.host.clone(),
            op_count: ops.len() as u64,
            conflict_count,
            source_entries: source.entries.len() as u64,
            target_entries: target.entries.len() as u64,
        },
        ops,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{Header, SCHEMA};

    fn snap(os: &str, entries: Vec<Entry>) -> Snapshot {
        Snapshot {
            header: Header {
                schema: SCHEMA, kind: "snapshot".into(), root: "/r".into(), host: "h".into(),
                os: os.into(), scanned_at_ms: 0, duration_ms: 0,
                entry_count: entries.len() as u64, hashed: true,
            },
            entries,
        }
    }
    fn file(path: &str, hash: &str) -> Entry {
        Entry { path: path.into(), kind: EntryKind::File, size: 1, mtime_ms: 0, hash: Some(hash.into()), file_id: None, mode: None, link: None, prev: None }
    }
    /// 带 mtime 的文件（冲突仲裁按 mtime）
    fn file_at(path: &str, hash: &str, mtime_ms: i64) -> Entry {
        Entry { mtime_ms, ..file(path, hash) }
    }
    fn sized(path: &str, hash: &str, size: u64) -> Entry {
        Entry { size, ..file(path, hash) }
    }
    /// archive 条目：当前 hash + 历史代
    fn arch(path: &str, hash: &str, prev: &[&str]) -> Entry {
        Entry {
            prev: if prev.is_empty() { None } else { Some(prev.iter().map(|s| s.to_string()).collect()) },
            ..file(path, hash)
        }
    }
    fn snap_named(os: &str, host: &str, entries: Vec<Entry>) -> Snapshot {
        let mut s = snap(os, entries);
        s.header.host = host.into();
        s
    }
    fn actions(plan: &Plan) -> Vec<(&str, &str)> {
        plan.ops
            .iter()
            .map(|o| {
                (
                    match o.action {
                        Action::Copy => "copy",
                        Action::Update => "update",
                        Action::Move => "move",
                        Action::Delete => "delete",
                        Action::DeleteDir => "deletedir",
                        Action::Chmod => "chmod",
                        Action::Conflict => "conflict",
                        Action::Note => "note",
                    },
                    o.path.as_str(),
                )
            })
            .collect()
    }

    // ---------- P2-5：空文件 / 歧义配对 ----------

    #[test]
    fn empty_files_are_never_paired_as_moves() {
        // 所有零长文件的 blake3 相同。过去它们会被配成一堆"重命名"，
        // 结果内容虽对，归因却是编的。syncthing 在 findRename 里直接排除 Size == 0。
        let e = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
        let s = snap("windows", vec![sized("new/a.py", e, 0), sized("new/b.py", e, 0)]);
        let t = snap("windows", vec![sized("old/x.py", e, 0), sized("old/y.py", e, 0)]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        assert!(
            !plan.ops.iter().any(|o| o.action == Action::Move),
            "zero-length files must never be paired as renames: {:?}",
            actions(&plan)
        );
        assert_eq!(plan.ops.iter().filter(|o| o.action == Action::Copy).count(), 2);
        assert_eq!(plan.ops.iter().filter(|o| o.action == Action::Delete).count(), 2);
    }

    #[test]
    fn ambiguous_move_is_labelled_as_such() {
        // 同内容的多个候选：配对结果内容正确，但 from 是任选的——reason 必须说实话
        let s = snap("windows", vec![sized("moved/one.bin", "h", 10)]);
        let t = snap(
            "windows",
            vec![sized("a/one.bin", "h", 10), sized("b/one.bin", "h", 10), sized("c/one.bin", "h", 10)],
        );
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let mv = plan.ops.iter().find(|o| o.action == Action::Move).expect("should still pair one");
        assert!(mv.reason.contains("ambiguous"), "reason must admit the ambiguity, got {:?}", mv.reason);
        assert!(mv.reason.contains('3'), "and say how many candidates: {:?}", mv.reason);
    }

    #[test]
    fn unambiguous_move_stays_clean() {
        let s = snap("windows", vec![sized("moved/one.bin", "h", 10)]);
        let t = snap("windows", vec![sized("one.bin", "h", 10)]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let mv = plan.ops.iter().find(|o| o.action == Action::Move).unwrap();
        assert!(!mv.reason.contains("ambiguous"), "a single candidate must not be flagged: {:?}", mv.reason);
    }

    // ---------- P1-3：archive 多代归因 ----------

    #[test]
    fn a_side_that_is_merely_behind_is_not_a_conflict() {
        // archive 已推进到 H2；source 改到 H3，target 还停在 H1（上次没同步成功）。
        // 过去两边都 != archive 当前代 → 误报 both-changed。
        let s = snap("windows", vec![file("f.txt", "H3")]);
        let t = snap("macos", vec![file("f.txt", "H1")]);
        let a = snap("windows", vec![arch("f.txt", "H2", &["H1", "H0"])]);
        let plan = compare(&s, &t, "sync", Some(&a), false, &CompareOptions::default());
        assert_eq!(plan.header.conflict_count, 0, "being behind is not concurrent editing: {:?}", actions(&plan));
        let up = plan.ops.iter().find(|o| o.action == Action::Update).expect("should propagate");
        assert_eq!(up.side, Side::Target);
        assert_eq!(up.hash.as_deref(), Some("H3"));
    }

    #[test]
    fn genuinely_novel_content_on_both_sides_is_still_a_conflict() {
        // 两边的内容 archive 都没见过 → 这才是真并发修改，绝不能被多代逻辑放过
        let s = snap("windows", vec![file("f.txt", "X")]);
        let t = snap("macos", vec![file("f.txt", "Y")]);
        let a = snap("windows", vec![arch("f.txt", "H2", &["H1", "H0"])]);
        let plan = compare(&s, &t, "sync", Some(&a), false, &CompareOptions::default());
        assert_eq!(plan.header.conflict_count, 1, "{:?}", actions(&plan));
    }

    #[test]
    fn newer_generation_wins_when_both_sides_are_behind() {
        // source 停在第 1 代、target 停在第 2 代 → source 较新，向 target 传播
        let s = snap("windows", vec![file("f.txt", "H1")]);
        let t = snap("macos", vec![file("f.txt", "H0")]);
        let a = snap("windows", vec![arch("f.txt", "H2", &["H1", "H0"])]);
        let plan = compare(&s, &t, "sync", Some(&a), false, &CompareOptions::default());
        assert_eq!(plan.header.conflict_count, 0);
        let up = plan.ops.iter().find(|o| o.action == Action::Update).unwrap();
        assert_eq!(up.side, Side::Target);
        assert!(up.reason.contains("behind-by-generations"), "{}", up.reason);
    }

    #[test]
    fn roll_generations_builds_the_history_chain() {
        use crate::table::roll_generations;
        let old = vec![arch("f.txt", "H1", &["H0"])];
        let mut fresh = vec![file("f.txt", "H2")];
        roll_generations(&mut fresh, &old);
        assert_eq!(fresh[0].prev.as_ref().unwrap(), &vec!["H1".to_string(), "H0".to_string()]);

        // 内容没变时不该把同一个 hash 灌进历史
        let mut same = vec![file("f.txt", "H1")];
        roll_generations(&mut same, &old);
        assert_eq!(same[0].prev.as_ref().unwrap(), &vec!["H0".to_string()]);
    }

    // ---------- P1-2：冲突副本 ----------

    #[test]
    fn conflict_policy_report_is_the_default_and_changes_nothing() {
        let s = snap_named("windows", "WIN", vec![file_at("f.txt", "X", 200)]);
        let t = snap_named("macos", "MAC", vec![file_at("f.txt", "Y", 100)]);
        let plan = compare(&s, &t, "sync", None, false, &CompareOptions::default());
        assert_eq!(plan.header.conflict_count, 1);
        assert!(!plan.ops.iter().any(|o| o.action == Action::Move), "report policy must not touch anything");
    }

    #[test]
    fn conflict_copy_keeps_the_loser_and_lands_the_winner() {
        let s = snap_named("windows", "WIN", vec![file_at("doc/report.pdf", "NEW", 5_000)]);
        let t = snap_named("macos", "MAC", vec![file_at("doc/report.pdf", "OLD", 1_000)]);
        let opts = CompareOptions { conflict: ConflictPolicy::Copy, ..Default::default() };
        let plan = compare(&s, &t, "sync", None, false, &opts);

        // 败方（target，mtime 旧）先改名留档
        let mv = plan.ops.iter().find(|o| o.action == Action::Move).expect("loser must be kept");
        assert_eq!(mv.side, Side::Target);
        assert_eq!(mv.from.as_deref(), Some("doc/report.pdf"));
        assert!(mv.path.starts_with("doc/report.sync-conflict-"), "{}", mv.path);
        assert!(mv.path.ends_with(".pdf"), "extension must be preserved: {}", mv.path);
        // 胜方内容落到 target
        let up = plan.ops.iter().find(|o| o.action == Action::Update && o.path == "doc/report.pdf").unwrap();
        assert_eq!(up.hash.as_deref(), Some("NEW"));
        // 原冲突行降级为可审计的 note，不再计入冲突数
        assert_eq!(plan.header.conflict_count, 0);
        assert!(plan.ops.iter().any(|o| o.action == Action::Note && o.reason.starts_with("auto-resolved")));
    }

    #[test]
    fn conflict_newer_overwrites_without_a_copy() {
        let s = snap_named("windows", "WIN", vec![file_at("f.txt", "NEW", 900)]);
        let t = snap_named("macos", "MAC", vec![file_at("f.txt", "OLD", 100)]);
        let opts = CompareOptions { conflict: ConflictPolicy::Newer, ..Default::default() };
        let plan = compare(&s, &t, "sync", None, false, &opts);
        assert!(!plan.ops.iter().any(|o| o.action == Action::Move), "newer policy keeps no copy");
        let up = plan.ops.iter().find(|o| o.action == Action::Update).unwrap();
        assert_eq!(up.side, Side::Target);
        assert_eq!(up.hash.as_deref(), Some("NEW"));
    }

    #[test]
    fn conflict_resolution_respects_the_older_side_winning() {
        // target 更新 → 胜方是 target，副本与覆盖都发生在 source 侧
        let s = snap_named("windows", "WIN", vec![file_at("f.txt", "OLD", 100)]);
        let t = snap_named("macos", "MAC", vec![file_at("f.txt", "NEW", 900)]);
        let opts = CompareOptions { conflict: ConflictPolicy::Copy, ..Default::default() };
        let plan = compare(&s, &t, "sync", None, false, &opts);
        let mv = plan.ops.iter().find(|o| o.action == Action::Move).unwrap();
        assert_eq!(mv.side, Side::Source);
        let up = plan.ops.iter().find(|o| o.action == Action::Update && o.path == "f.txt").unwrap();
        assert_eq!(up.side, Side::Source);
        assert_eq!(up.hash.as_deref(), Some("NEW"));
    }

    #[test]
    fn delete_versus_change_conflicts_are_never_auto_resolved() {
        // "对面删了但我改了" —— 自动仲裁"删还是留"太危险，任何策略下都只报告
        let s = snap_named("windows", "WIN", vec![file("f.txt", "CHANGED")]);
        let t = snap_named("macos", "MAC", Vec::new());
        let a = snap("windows", vec![file("f.txt", "ORIGINAL")]);
        let opts = CompareOptions { conflict: ConflictPolicy::Copy, ..Default::default() };
        let plan = compare(&s, &t, "sync", Some(&a), false, &opts);
        assert_eq!(plan.header.conflict_count, 1, "{:?}", actions(&plan));
        assert!(plan.ops.iter().any(|o| o.reason.contains("deleted-on-target-but-changed-on-source")));
    }

    #[test]
    fn conflict_names_are_well_formed() {
        let n = conflict_name("a/b/report.pdf", "WIN 01", 1_769_000_000_000);
        assert!(n.starts_with("a/b/report.sync-conflict-"), "{n}");
        assert!(n.ends_with("-WIN-01.pdf"), "host must be sanitised and extension kept: {n}");
        assert!(is_conflict_copy(&n));
        // 隐藏文件没有扩展名可言
        let h = conflict_name(".gitignore", "H", 0);
        assert!(h.starts_with(".gitignore.sync-conflict-"), "{h}");
        assert!(!is_conflict_copy("a/b/normal.pdf"));
    }

    #[test]
    fn conflict_copies_over_the_limit_are_pruned() {
        let mut entries = vec![file("f.txt", "SAME")];
        for i in 1..=4 {
            entries.push(file(&format!("f.sync-conflict-2026070{i}-120000-MAC.txt"), &format!("c{i}")));
        }
        let s = snap_named("windows", "WIN", vec![file("f.txt", "SAME")]);
        let t = snap_named("macos", "MAC", entries);
        let opts = CompareOptions { conflict: ConflictPolicy::Copy, max_conflicts: 2, ..Default::default() };
        let plan = compare(&s, &t, "sync", None, false, &opts);
        let pruned: Vec<&str> = plan
            .ops
            .iter()
            .filter(|o| o.reason.contains("conflict-copy-over-limit"))
            .map(|o| o.path.as_str())
            .collect();
        assert_eq!(pruned.len(), 2, "4 copies, limit 2 -> drop the 2 oldest: {pruned:?}");
        assert!(pruned.iter().all(|p| p.contains("20260701") || p.contains("20260702")), "{pruned:?}");
    }

    // ---------- P2-4：unix 权限位 ----------

    #[test]
    fn mode_only_difference_produces_a_chmod_not_a_recopy() {
        let mut se = file("run.sh", "SAME");
        se.mode = Some(0o755);
        let mut te = file("run.sh", "SAME");
        te.mode = Some(0o644);
        let s = snap("macos", vec![se]);
        let t = snap("linux", vec![te]);
        let opts = CompareOptions { sync_mode: true, ..Default::default() };
        let plan = compare(&s, &t, "mirror", None, false, &opts);
        assert_eq!(actions(&plan), vec![("chmod", "run.sh")], "content is identical; only the bits differ");
        assert_eq!(plan.ops[0].mode, Some(0o755));
    }

    #[test]
    fn mode_is_ignored_unless_enabled_and_both_sides_are_unix() {
        let mut se = file("run.sh", "SAME");
        se.mode = Some(0o755);
        let mut te = file("run.sh", "SAME");
        te.mode = Some(0o644);
        // 默认关
        let plan = compare(&snap("macos", vec![se.clone()]), &snap("linux", vec![te.clone()]), "mirror", None, false, &CompareOptions::default());
        assert!(plan.ops.is_empty());
        // Windows 一侧没有 mode，开了也不该报差异
        let opts = CompareOptions { sync_mode: true, ..Default::default() };
        let plan2 = compare(&snap("macos", vec![se]), &snap("windows", vec![te]), "mirror", None, false, &opts);
        assert!(plan2.ops.is_empty(), "{:?}", actions(&plan2));
    }

    #[test]
    fn copies_carry_the_source_mode_when_enabled() {
        let mut se = file("new.sh", "H");
        se.mode = Some(0o755);
        let s = snap("macos", vec![se]);
        let t = snap("linux", Vec::new());
        let opts = CompareOptions { sync_mode: true, ..Default::default() };
        let plan = compare(&s, &t, "mirror", None, false, &opts);
        assert_eq!(plan.ops.len(), 1);
        assert_eq!(plan.ops[0].mode, Some(0o755), "a fresh copy must land with the right bits in one step");
    }

    // ---------- P2-3：大小写撞名 ----------

    #[test]
    fn case_sensitive_mode_flags_a_write_that_would_clobber_a_case_twin() {
        // case_sensitive = true 时 Foo.txt 与 foo.txt 是两个文件，
        // 但 NTFS/APFS 上写前者会静默覆盖后者。
        let s = snap("windows", vec![file("Foo.txt", "A"), file("foo.txt", "B")]);
        let t = snap("windows", vec![file("foo.txt", "B")]);
        let opts = CompareOptions { case_insensitive: false, ..Default::default() };
        let plan = compare(&s, &t, "mirror", None, false, &opts);
        let c = plan.ops.iter().find(|o| o.path == "Foo.txt").expect("Foo.txt must be planned somehow");
        assert_eq!(c.action, Action::Conflict, "{:?}", c.reason);
        assert!(c.reason.contains("case-collision"), "{}", c.reason);
    }

    #[test]
    fn nfc_nfd_paths_match() {
        // "café" NFC (U+00E9) vs NFD (e + U+0301)：同一文件，不该产生任何 op
        let nfc = "caf\u{00e9}.txt";
        let nfd = "cafe\u{0301}.txt";
        let s = snap("windows", vec![file(nfc, "h1")]);
        let t = snap("macos", vec![file(nfd, "h1")]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        assert_eq!(plan.ops.len(), 0, "NFC/NFD spellings of the same name must match");
    }

    #[test]
    fn case_insensitive_match_and_opt_out() {
        let s = snap("windows", vec![file("Readme.md", "h1")]);
        let t = snap("macos", vec![file("readme.md", "h1")]);
        assert_eq!(compare(&s, &t, "mirror", None, false, &CompareOptions { case_insensitive: true, ..Default::default() }).ops.len(), 0);
        // 大小写敏感时：同 hash 的大小写双胞胎被移动检测配对成一次 rename——比复制+删除更聪明
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions { case_insensitive: false, ..Default::default() });
        assert_eq!(plan.ops.len(), 1);
        assert_eq!(plan.ops[0].action, Action::Move);
    }

    #[test]
    fn update_keeps_target_spelling() {
        let s = snap("windows", vec![file("CAF\u{00c9}.TXT", "new")]);
        let t = snap("macos", vec![file("cafe\u{0301}.txt", "old")]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        assert_eq!(plan.ops.len(), 1);
        assert_eq!(plan.ops[0].action, Action::Update);
        assert_eq!(plan.ops[0].path, "cafe\u{0301}.txt", "update must use target's own spelling");
    }

    #[test]
    fn illegal_windows_names_become_conflicts() {
        let s = snap("macos", vec![file("aux.log", "h1"), file("ok.txt", "h2"), file("bad. /x", "h3")]);
        let t = snap("windows", Vec::new());
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let conflicts: Vec<_> = plan.ops.iter().filter(|o| o.action == Action::Conflict).collect();
        assert_eq!(conflicts.len(), 2, "aux.log and 'bad. ' segment must be flagged");
        assert!(plan.ops.iter().any(|o| o.action == Action::Copy && o.path == "ok.txt"));
    }

    // ---------- sync-with-archive 分类矩阵 ----------
    // 状态记号：E=存在且内容 x，∅=不存在。archive = 上次同步的共识状态。

    fn plan_sync(s: Vec<Entry>, t: Vec<Entry>, a: Option<Vec<Entry>>) -> Plan {
        let s = snap("windows", s);
        let t = snap("macos", t);
        let a = a.map(|e| snap("windows", e));
        compare(&s, &t, "sync", a.as_ref(), false, &CompareOptions::default())
    }
    fn one(plan: &Plan) -> &Op {
        assert_eq!(plan.ops.len(), 1, "expected exactly 1 op, got: {:?}", plan.ops);
        &plan.ops[0]
    }

    #[test]
    fn matrix_equal_no_op() {
        let p = plan_sync(vec![file("a", "h")], vec![file("a", "h")], Some(vec![file("a", "h")]));
        assert_eq!(p.ops.len(), 0);
    }

    #[test]
    fn matrix_source_changed_propagates_to_target() {
        let p = plan_sync(vec![file("a", "h2")], vec![file("a", "h1")], Some(vec![file("a", "h1")]));
        let op = one(&p);
        assert_eq!((op.side.clone(), op.action.clone()), (Side::Target, Action::Update));
    }

    #[test]
    fn matrix_target_changed_propagates_to_source() {
        let p = plan_sync(vec![file("a", "h1")], vec![file("a", "h2")], Some(vec![file("a", "h1")]));
        let op = one(&p);
        assert_eq!((op.side.clone(), op.action.clone()), (Side::Source, Action::Update));
    }

    #[test]
    fn matrix_both_changed_conflict() {
        let p = plan_sync(vec![file("a", "h2")], vec![file("a", "h3")], Some(vec![file("a", "h1")]));
        assert_eq!(one(&p).action, Action::Conflict);
        assert_eq!(p.header.conflict_count, 1);
    }

    #[test]
    fn matrix_target_deleted_propagates_deletion() {
        // archive 有、target 没、source 未改 → source 上删除
        let p = plan_sync(vec![file("a", "h1")], vec![], Some(vec![file("a", "h1")]));
        let op = one(&p);
        assert_eq!((op.side.clone(), op.action.clone()), (Side::Source, Action::Delete));
    }

    #[test]
    fn matrix_delete_vs_edit_conflict() {
        // target 删了它，但 source 改过 → 删改冲突，绝不静默删
        let p = plan_sync(vec![file("a", "h2")], vec![], Some(vec![file("a", "h1")]));
        assert_eq!(one(&p).action, Action::Conflict);
    }

    #[test]
    fn matrix_new_on_source_copies() {
        let p = plan_sync(vec![file("a", "h1")], vec![], Some(vec![]));
        let op = one(&p);
        assert_eq!((op.side.clone(), op.action.clone()), (Side::Target, Action::Copy));
    }

    #[test]
    fn matrix_move_on_source_replayed_on_target() {
        // source 把 a 挪成 b；target/archive 还是 a → target 上重演 move
        let p = plan_sync(vec![file("b", "h1")], vec![file("a", "h1")], Some(vec![file("a", "h1")]));
        let op = one(&p);
        assert_eq!(op.action, Action::Move);
        assert_eq!(op.side, Side::Target);
        assert_eq!(op.from.as_deref(), Some("a"));
        assert_eq!(op.path, "b");
    }

    #[test]
    fn matrix_no_archive_differ_is_conflict_and_adds_flow_both_ways() {
        let p = plan_sync(vec![file("a", "h1"), file("s", "hs")], vec![file("a", "h2"), file("t", "ht")], None);
        assert!(p.ops.iter().any(|o| o.action == Action::Conflict && o.path == "a"));
        assert!(p.ops.iter().any(|o| o.action == Action::Copy && o.side == Side::Target && o.path == "s"));
        assert!(p.ops.iter().any(|o| o.action == Action::Copy && o.side == Side::Source && o.path == "t"));
        assert!(!p.ops.iter().any(|o| o.action == Action::Delete), "no-archive sync must never delete");
    }

    #[test]
    fn matrix_enrich_never_deletes_or_downgrades() {
        let s = snap("windows", vec![file("only-src", "h1")]);
        let mut old = file("shared", "h-old");
        old.mtime_ms = 0;
        let mut newer_on_target = file("shared", "h-new");
        newer_on_target.mtime_ms = 999_999;
        let t = snap("macos", vec![newer_on_target, file("only-tgt", "hx")]);
        let s = Snapshot { header: s.header, entries: vec![s.entries[0].clone(), old] };
        let p = compare(&s, &t, "enrich", None, false, &CompareOptions::default());
        assert!(p.ops.iter().any(|o| o.action == Action::Copy && o.path == "only-src"));
        assert!(!p.ops.iter().any(|o| o.action == Action::Delete));
        assert!(!p.ops.iter().any(|o| o.action == Action::Update), "enrich must not downgrade newer target");
    }

    #[test]
    fn reverse_op_semantics() {
        let copy = Op { side: Side::Target, action: Action::Copy, path: "x".into(), from: None, size: Some(5), mtime_ms: Some(1), hash: None, link: None, mode: None, reason: "only-in-source".into() };
        let r = reverse_op(&copy).unwrap();
        assert_eq!((r.side, r.action), (Side::Source, Action::Delete));

        let del = Op { side: Side::Target, action: Action::Delete, path: "x".into(), from: None, size: Some(5), mtime_ms: None, hash: None, link: None, mode: None, reason: "gone-from-source".into() };
        let r = reverse_op(&del).unwrap();
        assert_eq!((r.side, r.action), (Side::Source, Action::Copy));

        let upd = Op { side: Side::Target, action: Action::Update, path: "x".into(), from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "differs".into() };
        let r = reverse_op(&upd).unwrap();
        assert_eq!((r.side, r.action), (Side::Source, Action::Update));

        let mv = Op { side: Side::Target, action: Action::Move, path: "b".into(), from: Some("a".into()), size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "m".into() };
        assert!(reverse_op(&mv).is_none());
    }

    #[test]
    fn normalization_twins_reported_not_merged() {
        let s = snap("linux", vec![file("caf\u{00e9}.txt", "h1"), file("cafe\u{0301}.txt", "h2")]);
        let t = snap("windows", vec![file("caf\u{00e9}.txt", "h1")]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        assert!(plan.ops.iter().any(|o| o.action == Action::Note && o.reason.contains("duplicate-after-normalization")));
    }

    // ---------- 证据层 ----------

    #[test]
    fn evidence_reports_both_sides_and_equal_count() {
        // same 两侧一致；upd 两侧都在但内容不同；only_s 只在 source；only_t 只在 target
        let s = snap("windows", vec![
            Entry { size: 10, mtime_ms: 1_000, ..file("same.txt", "h0") },
            Entry { size: 30, mtime_ms: 9_000, ..file("upd.txt", "hs") },
            Entry { size: 7, mtime_ms: 5_000, ..file("only_s.txt", "h1") },
        ]);
        let t = snap("windows", vec![
            Entry { size: 10, mtime_ms: 1_000, ..file("same.txt", "h0") },
            Entry { size: 20, mtime_ms: 2_000, ..file("upd.txt", "ht") },
            Entry { size: 4, mtime_ms: 3_000, ..file("only_t.txt", "h2") },
        ]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let ev = evidence(&s, &t, &plan, &CompareOptions::default());
        assert_eq!(ev.metas.len(), plan.ops.len(), "证据数组必须与 ops 一一对应");
        assert_eq!(ev.equal_count, 1);
        assert_eq!(ev.equal_bytes, 10);

        let by = |name: &str| {
            let i = plan.ops.iter().position(|o| o.path == name).expect(name);
            ev.metas[i].clone()
        };
        // update 行：两侧都要有各自的实测值——今天 Op.size/mtime 只带得动 source 那一份
        let m = by("upd.txt");
        assert_eq!(m.src.unwrap().size, 30);
        assert_eq!(m.src.unwrap().mtime_ms, 9_000);
        assert_eq!(m.dst.unwrap().size, 20);
        assert_eq!(m.dst.unwrap().mtime_ms, 2_000);
        // copy 行只有 source 侧存在
        let m = by("only_s.txt");
        assert_eq!(m.src.unwrap().size, 7);
        assert!(m.dst.is_none());
        // delete 行只有 target 侧存在
        let m = by("only_t.txt");
        assert!(m.src.is_none());
        assert_eq!(m.dst.unwrap().size, 4);
    }

    #[test]
    fn same_page_lists_only_equal_files_and_pages() {
        let mk = |n: usize, h: &str| Entry { size: n as u64, mtime_ms: n as i64 * 1000, ..file(&format!("d{}/f{n}.bin", n % 3), h) };
        let s = snap("windows", (0..10).map(|n| mk(n, "same")).collect());
        // 后 3 个在 target 侧内容不同 → 不算相同
        let t = snap("windows", (0..10).map(|n| mk(n, if n >= 7 { "diff" } else { "same" })).collect());
        let copts = CompareOptions::default();
        let (total, rows) = same_page(&s, &t, &copts, "", 0, 100);
        assert_eq!(total, 7);
        assert_eq!(rows.len(), 7);
        // 分页
        let (total, rows) = same_page(&s, &t, &copts, "", 5, 100);
        assert_eq!(total, 7, "total 是过滤后的总数，与分页窗口无关");
        assert_eq!(rows.len(), 2);
        let (_t, rows) = same_page(&s, &t, &copts, "", 0, 3);
        assert_eq!(rows.len(), 3);
        // 子串过滤（大小写不敏感）
        let (total, rows) = same_page(&s, &t, &copts, "D1/", 0, 100);
        assert_eq!(total as usize, rows.len());
        assert!(rows.iter().all(|r| r.path.starts_with("d1/")));
        // 两侧时间都要带出来（内容相同但时间戳可以不同）
        assert!(rows.iter().all(|r| r.mtime_ms == r.other_mtime_ms));
    }

    #[test]
    fn evidence_follows_move_naming_on_each_side() {
        // 同内容改名：source 已叫 b.bin，target 还叫 a.bin —— 两侧各查各的名字
        let s = snap("windows", vec![Entry { size: 42, mtime_ms: 8_000, ..file("b.bin", "hm") }]);
        let t = snap("windows", vec![Entry { size: 42, mtime_ms: 4_000, ..file("a.bin", "hm") }]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let i = plan.ops.iter().position(|o| o.action == Action::Move).expect("move");
        let ev = evidence(&s, &t, &plan, &CompareOptions::default());
        assert_eq!(ev.metas[i].src.unwrap().mtime_ms, 8_000, "source 侧按新名字 b.bin 查");
        assert_eq!(ev.metas[i].dst.unwrap().mtime_ms, 4_000, "target 侧按旧名字 a.bin 查");
    }
}
