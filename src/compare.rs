//! compare：N 表比对 → 行动计划（要求 2/3）。
//! 模式语义：
//!   mirror  —— source 为 master：target 补齐/更新/删除，完全向 source 看齐；
//!              移动检测：source 独有路径与 target 独有路径按 (hash,size) 配对 → 生成 move（治 FFS 删+增）
//!   enrich  —— 只增不删：target 缺的补上，source 较新的更新过去；不删除、不移动、不回退
//!   sync    —— 双向。带 --archive（上次同步存档，Unison 思路）时可区分"删除 vs 新增"并
//!              归因移动；无 archive 时退化为安全模式：双向补齐 + 差异报冲突 + 疑似移动只报告
//! 相等判定：双方都有 hash → 按 hash；否则 size 相等且 |Δmtime| <= 2s（FAT/SMB 粒度）

use crate::table::{now_ms, Entry, EntryKind, Snapshot};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const MTIME_SLACK_MS: i64 = 2000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Copy,      // 从对侧 root 的同名相对路径复制过来
    Update,    // 覆盖（旧文件进回收目录）
    Move,      // 本侧 from -> path
    Delete,
    DeleteDir, // 仅当已空
    Conflict,  // 需要人工定向
    Note,      // 信息（如疑似移动但无法归因）
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime_ms: Option<i64>,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PlanHeader {
    pub schema: u32,
    pub kind: String, // "plan"
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

fn files_equal(a: &Entry, b: &Entry) -> bool {
    if let (Some(ha), Some(hb)) = (&a.hash, &b.hash) {
        return ha == hb;
    }
    a.size == b.size && (a.mtime_ms - b.mtime_ms).abs() <= MTIME_SLACK_MS
}

fn map_of<'a>(snap: &'a Snapshot, kind: EntryKind) -> BTreeMap<&'a str, &'a Entry> {
    snap.entries.iter().filter(|e| e.kind == kind).map(|e| (e.path.as_str(), e)).collect()
}

/// 在"待复制(add)"与"待删除(del)"里按 (hash,size) 配对出移动。返回 (moves(from,to,size), 剩余 adds, 剩余 dels)
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
                    // 优先同文件名（整目录改名的常见情形），否则任取（内容一致，语义相同）
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
    ops.push(Op { side, action: Action::Copy, path: e.path.clone(), from: None, size: Some(e.size), mtime_ms: Some(e.mtime_ms), reason: reason.into() });
}

pub fn compare(source: &Snapshot, target: &Snapshot, mode: &str, archive: Option<&Snapshot>, resolve_newer: bool) -> Plan {
    let s_files = map_of(source, EntryKind::File);
    let t_files = map_of(target, EntryKind::File);
    let s_dirs = map_of(source, EntryKind::Dir);
    let t_dirs = map_of(target, EntryKind::Dir);
    let both_hashed = source.header.hashed && target.header.hashed;
    let mut ops: Vec<Op> = Vec::new();

    match mode {
        "mirror" | "enrich" => {
            let mut adds: Vec<&Entry> = Vec::new();
            let mut dels: Vec<&Entry> = Vec::new();
            for (&p, &se) in &s_files {
                match t_files.get(p) {
                    None => adds.push(se),
                    Some(&te) => {
                        if !files_equal(se, te) && (mode == "mirror" || se.mtime_ms > te.mtime_ms + MTIME_SLACK_MS) {
                            let reason = if mode == "mirror" { "differs-master-wins" } else { "source-newer" };
                            ops.push(Op { side: Side::Target, action: Action::Update, path: se.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), reason: reason.into() });
                        }
                    }
                }
            }
            if mode == "mirror" {
                for (&p, &te) in &t_files {
                    if !s_files.contains_key(p) {
                        dels.push(te);
                    }
                }
                let (moves, rest_adds, rest_dels) = if both_hashed {
                    detect_moves(adds, dels)
                } else {
                    (Vec::new(), adds, dels)
                };
                for (from, to, size) in moves {
                    ops.push(Op { side: Side::Target, action: Action::Move, path: to, from: Some(from), size: Some(size), mtime_ms: None, reason: "move-detected-by-hash".into() });
                }
                for a in rest_adds { push_copy(&mut ops, Side::Target, a, "only-in-source"); }
                for d in rest_dels {
                    ops.push(Op { side: Side::Target, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, reason: "gone-from-source".into() });
                }
                for (&p, _) in &t_dirs {
                    if !s_dirs.contains_key(p) {
                        ops.push(Op { side: Side::Target, action: Action::DeleteDir, path: p.to_string(), from: None, size: None, mtime_ms: None, reason: "dir-gone-from-source".into() });
                    }
                }
            } else {
                for a in adds { push_copy(&mut ops, Side::Target, a, "only-in-source"); }
            }
        }
        "sync" => {
            let arch_files: BTreeMap<&str, &Entry> = archive.map(|a| map_of(a, EntryKind::File)).unwrap_or_default();
            let has_archive = archive.is_some();
            let mut s_adds: Vec<&Entry> = Vec::new();          // source 侧新增（待复制到 target）
            let mut t_adds: Vec<&Entry> = Vec::new();          // target 侧新增（待复制到 source）
            let mut del_on_target: Vec<&Entry> = Vec::new();   // source 已删 → 待删 target
            let mut del_on_source: Vec<&Entry> = Vec::new();   // target 已删 → 待删 source

            for (&p, &se) in &s_files {
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
                                ops.push(Op { side: Side::Source, action: Action::Update, path: p.to_string(), from: None, size: Some(te.size), mtime_ms: Some(te.mtime_ms), reason: "target-changed".into() });
                            } else if t_unchanged && !s_unchanged {
                                ops.push(Op { side: Side::Target, action: Action::Update, path: p.to_string(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), reason: "source-changed".into() });
                            } else {
                                ops.push(Op { side: Side::Target, action: Action::Conflict, path: p.to_string(), from: None, size: None, mtime_ms: None, reason: "both-changed".into() });
                            }
                        } else if resolve_newer {
                            let (side, e): (Side, &Entry) = if se.mtime_ms >= te.mtime_ms { (Side::Target, se) } else { (Side::Source, te) };
                            ops.push(Op { side, action: Action::Update, path: p.to_string(), from: None, size: Some(e.size), mtime_ms: Some(e.mtime_ms), reason: "differs-newer-wins".into() });
                        } else {
                            ops.push(Op { side: Side::Target, action: Action::Conflict, path: p.to_string(), from: None, size: None, mtime_ms: None, reason: "differs-no-archive".into() });
                        }
                    }
                    None => {
                        if has_archive {
                            if let Some(&r) = arch_files.get(p) {
                                if files_equal(se, r) {
                                    del_on_source.push(se); // target 删了它，且 source 没改过 → 传播删除
                                } else {
                                    ops.push(Op { side: Side::Target, action: Action::Conflict, path: p.to_string(), from: None, size: None, mtime_ms: None, reason: "deleted-on-target-but-changed-on-source".into() });
                                }
                                continue;
                            }
                        }
                        s_adds.push(se);
                    }
                }
            }
            for (&p, &te) in &t_files {
                if s_files.contains_key(p) {
                    continue;
                }
                if has_archive {
                    if let Some(&r) = arch_files.get(p) {
                        if files_equal(te, r) {
                            del_on_target.push(te); // source 删了它，且 target 没改过 → 传播删除
                        } else {
                            ops.push(Op { side: Side::Target, action: Action::Conflict, path: p.to_string(), from: None, size: None, mtime_ms: None, reason: "deleted-on-source-but-changed-on-target".into() });
                        }
                        continue;
                    }
                }
                t_adds.push(te);
            }

            if has_archive && both_hashed {
                // source 上的移动 = source 侧 (新增, source已删) 配对 → 在 target 重演；反之亦然
                let (mv_on_target, rest_s_adds, rest_del_t) = detect_moves(s_adds, del_on_target);
                for (from, to, size) in mv_on_target {
                    ops.push(Op { side: Side::Target, action: Action::Move, path: to, from: Some(from), size: Some(size), mtime_ms: None, reason: "move-on-source-replayed".into() });
                }
                let (mv_on_source, rest_t_adds, rest_del_s) = detect_moves(t_adds, del_on_source);
                for (from, to, size) in mv_on_source {
                    ops.push(Op { side: Side::Source, action: Action::Move, path: to, from: Some(from), size: Some(size), mtime_ms: None, reason: "move-on-target-replayed".into() });
                }
                for a in rest_s_adds { push_copy(&mut ops, Side::Target, a, "added-on-source"); }
                for a in rest_t_adds { push_copy(&mut ops, Side::Source, a, "added-on-target"); }
                for d in rest_del_t {
                    ops.push(Op { side: Side::Target, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, reason: "deleted-on-source".into() });
                }
                for d in rest_del_s {
                    ops.push(Op { side: Side::Source, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, reason: "deleted-on-target".into() });
                }
            } else {
                // 无 archive（或缺 hash）：只做安全的双向补齐；疑似移动仅报告，删除一律不做
                if both_hashed {
                    let t_only: HashMap<&str, &str> = t_adds.iter()
                        .filter_map(|e| e.hash.as_deref().map(|h| (h, e.path.as_str())))
                        .collect();
                    for a in &s_adds {
                        if let Some(h) = a.hash.as_deref() {
                            if let Some(&other) = t_only.get(h) {
                                ops.push(Op { side: Side::Target, action: Action::Note, path: a.path.clone(), from: Some(other.to_string()), size: Some(a.size), mtime_ms: None, reason: "possible-move-needs-archive".into() });
                            }
                        }
                    }
                }
                for a in s_adds { push_copy(&mut ops, Side::Target, a, "only-in-source"); }
                for a in t_adds { push_copy(&mut ops, Side::Source, a, "only-in-target"); }
                for d in del_on_target {
                    ops.push(Op { side: Side::Target, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, reason: "deleted-on-source".into() });
                }
                for d in del_on_source {
                    ops.push(Op { side: Side::Source, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, reason: "deleted-on-target".into() });
                }
            }
        }
        other => panic!("unknown mode: {other}"),
    }

    // 应用顺序：move → copy/update → delete → delete_dir(深→浅)；conflict/note 殿后展示
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
