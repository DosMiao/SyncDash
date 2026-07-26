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

#[derive(Clone, Copy)]
pub struct CompareOptions {
    /// 默认 true：NTFS 与 APFS 默认都大小写不敏感
    pub case_insensitive: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions { case_insensitive: true }
    }
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

fn detect_moves<'a>(
    adds: Vec<&'a Entry>,
    dels: Vec<&'a Entry>,
) -> (Vec<(String, String, u64)>, Vec<&'a Entry>, Vec<&'a Entry>) {
    let mut by_key: HashMap<(String, u64), Vec<&'a Entry>> = HashMap::new();
    for &d in &dels {
        if let Some(h) = &d.hash {
            by_key.entry((h.clone(), d.size)).or_default().push(d);
        }
    }
    let mut moves = Vec::new();
    let mut rest_adds = Vec::new();
    let mut used: HashSet<String> = HashSet::new();
    for a in adds {
        let mut matched = None;
        if let Some(h) = &a.hash {
            if let Some(cands) = by_key.get_mut(&(h.clone(), a.size)) {
                if !cands.is_empty() {
                    let pick = cands
                        .iter()
                        .position(|c| {
                            std::path::Path::new(&c.path).file_name()
                                == std::path::Path::new(&a.path).file_name()
                        })
                        .unwrap_or(0);
                    let c = cands.remove(pick);
                    used.insert(c.path.clone());
                    matched = Some((c.path.clone(), a.path.clone(), a.size));
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

fn push_copy(ops: &mut Vec<Op>, side: Side, e: &Entry, reason: &str) {
    ops.push(Op { side, action: Action::Copy, path: e.path.clone(), from: None, size: Some(e.size), mtime_ms: Some(e.mtime_ms), hash: e.hash.clone(), reason: reason.into() });
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
        ops.push(Op { side: Side::Source, action: Action::Note, path: d, from: None, size: None, mtime_ms: None, hash: None, reason: "duplicate-after-normalization (kept first; NFC/case twin)".into() });
    }
    for d in t_dups {
        ops.push(Op { side: Side::Target, action: Action::Note, path: d, from: None, size: None, mtime_ms: None, hash: None, reason: "duplicate-after-normalization (kept first; NFC/case twin)".into() });
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
                            ops.push(Op { side: Side::Target, action: Action::Update, path: te.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: se.hash.clone(), reason: reason.into() });
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
                for (from, to, size) in moves {
                    ops.push(Op { side: Side::Target, action: Action::Move, path: to, from: Some(from), size: Some(size), mtime_ms: None, hash: None, reason: "move-detected-by-hash".into() });
                }
                for a in rest_adds { push_copy(&mut ops, Side::Target, a, "only-in-source"); }
                for d in rest_dels {
                    ops.push(Op { side: Side::Target, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, reason: "gone-from-source".into() });
                }
                for (p, te) in &t_dirs {
                    if !s_dirs.contains_key(p) {
                        ops.push(Op { side: Side::Target, action: Action::DeleteDir, path: te.path.clone(), from: None, size: None, mtime_ms: None, hash: None, reason: "dir-gone-from-source".into() });
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
                            let r = arch_files.get(p).copied();
                            let s_unchanged = r.map(|r| files_equal(se, r)).unwrap_or(false);
                            let t_unchanged = r.map(|r| files_equal(te, r)).unwrap_or(false);
                            if s_unchanged && !t_unchanged {
                                ops.push(Op { side: Side::Source, action: Action::Update, path: se.path.clone(), from: None, size: Some(te.size), mtime_ms: Some(te.mtime_ms), hash: te.hash.clone(), reason: "target-changed".into() });
                            } else if t_unchanged && !s_unchanged {
                                ops.push(Op { side: Side::Target, action: Action::Update, path: te.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: se.hash.clone(), reason: "source-changed".into() });
                            } else {
                                ops.push(Op { side: Side::Target, action: Action::Conflict, path: se.path.clone(), from: None, size: None, mtime_ms: None, hash: None, reason: "both-changed".into() });
                            }
                        } else if resolve_newer {
                            if se.mtime_ms >= te.mtime_ms {
                                ops.push(Op { side: Side::Target, action: Action::Update, path: te.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: se.hash.clone(), reason: "differs-newer-wins".into() });
                            } else {
                                ops.push(Op { side: Side::Source, action: Action::Update, path: se.path.clone(), from: None, size: Some(te.size), mtime_ms: Some(te.mtime_ms), hash: te.hash.clone(), reason: "differs-newer-wins".into() });
                            }
                        } else {
                            ops.push(Op { side: Side::Target, action: Action::Conflict, path: se.path.clone(), from: None, size: None, mtime_ms: None, hash: None, reason: "differs-no-archive".into() });
                        }
                    }
                    None => {
                        if has_archive {
                            if let Some(&r) = arch_files.get(p) {
                                if files_equal(se, r) {
                                    del_on_source.push(se);
                                } else {
                                    ops.push(Op { side: Side::Target, action: Action::Conflict, path: se.path.clone(), from: None, size: None, mtime_ms: None, hash: None, reason: "deleted-on-target-but-changed-on-source".into() });
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
                            ops.push(Op { side: Side::Target, action: Action::Conflict, path: te.path.clone(), from: None, size: None, mtime_ms: None, hash: None, reason: "deleted-on-source-but-changed-on-target".into() });
                        }
                        continue;
                    }
                }
                t_adds.push(te);
            }

            if has_archive && both_hashed {
                let (mv_on_target, rest_s_adds, rest_del_t) = detect_moves(s_adds, del_on_target);
                for (from, to, size) in mv_on_target {
                    ops.push(Op { side: Side::Target, action: Action::Move, path: to, from: Some(from), size: Some(size), mtime_ms: None, hash: None, reason: "move-on-source-replayed".into() });
                }
                let (mv_on_source, rest_t_adds, rest_del_s) = detect_moves(t_adds, del_on_source);
                for (from, to, size) in mv_on_source {
                    ops.push(Op { side: Side::Source, action: Action::Move, path: to, from: Some(from), size: Some(size), mtime_ms: None, hash: None, reason: "move-on-target-replayed".into() });
                }
                for a in rest_s_adds { push_copy(&mut ops, Side::Target, a, "added-on-source"); }
                for a in rest_t_adds { push_copy(&mut ops, Side::Source, a, "added-on-target"); }
                for d in rest_del_t {
                    ops.push(Op { side: Side::Target, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, reason: "deleted-on-source".into() });
                }
                for d in rest_del_s {
                    ops.push(Op { side: Side::Source, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, reason: "deleted-on-target".into() });
                }
            } else {
                if both_hashed {
                    let t_only: HashMap<&str, &str> = t_adds.iter()
                        .filter_map(|e| e.hash.as_deref().map(|h| (h, e.path.as_str())))
                        .collect();
                    for a in &s_adds {
                        if let Some(h) = a.hash.as_deref() {
                            if let Some(&other) = t_only.get(h) {
                                ops.push(Op { side: Side::Target, action: Action::Note, path: a.path.clone(), from: Some(other.to_string()), size: Some(a.size), mtime_ms: None, hash: None, reason: "possible-move-needs-archive".into() });
                            }
                        }
                    }
                }
                for a in s_adds { push_copy(&mut ops, Side::Target, a, "only-in-source"); }
                for a in t_adds { push_copy(&mut ops, Side::Source, a, "only-in-target"); }
                for d in del_on_target {
                    ops.push(Op { side: Side::Target, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, reason: "deleted-on-source".into() });
                }
                for d in del_on_source {
                    ops.push(Op { side: Side::Source, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, reason: "deleted-on-target".into() });
                }
            }
        }
        other => panic!("unknown mode: {other}"),
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
        Action::Delete => 2,
        Action::DeleteDir => 3,
        Action::Conflict | Action::Note => 4,
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
        Entry { path: path.into(), kind: EntryKind::File, size: 1, mtime_ms: 0, hash: Some(hash.into()), file_id: None, mode: None }
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
        assert_eq!(compare(&s, &t, "mirror", None, false, &CompareOptions { case_insensitive: true }).ops.len(), 0);
        // 大小写敏感时：同 hash 的大小写双胞胎被移动检测配对成一次 rename——比复制+删除更聪明
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions { case_insensitive: false });
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

    #[test]
    fn normalization_twins_reported_not_merged() {
        let s = snap("linux", vec![file("caf\u{00e9}.txt", "h1"), file("cafe\u{0301}.txt", "h2")]);
        let t = snap("windows", vec![file("caf\u{00e9}.txt", "h1")]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        assert!(plan.ops.iter().any(|o| o.action == Action::Note && o.reason.contains("duplicate-after-normalization")));
    }
}
