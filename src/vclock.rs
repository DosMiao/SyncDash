//! 版本向量（P2-1）。语义 1:1 对照 syncthing `lib/protocol/vector.go`，用 Rust 重写。
//!
//! 为什么需要它：archive（上次同步快照）只能回答"相对上一次，这一侧变了没有"，
//! 两个节点以上就会退化——A→B 同步后 B→C，C 再改回来，谁也说不清是谁基于谁改的。
//! 版本向量把"因果关系"直接编码进每个文件的版本里：
//!   - `Greater` / `Lesser`：一方是另一方的后代 → 单边传播，不是冲突
//!   - `Concurrent`：互不为祖先 → **真冲突**
//!
//! 计数器取 `max(old+1, unix_now)`（syncthing `updateWithNow`）：用时间戳做下界，
//! 即使某个节点的库被旧备份覆盖，计数器也不会倒退回已用过的值。
//!
//! **当前定位**：数学核心与节点身份已完成并通过测试，`compare` 在
//! `archive_format = "index"` 时启用（见 config.rs）。默认仍走 archive 快照路径——
//! 真 N 向要求每次 apply 后精确维护向量，那是 v1.0 的收敛性工程。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 节点短 ID。由本机 node-id 文件派生，稳定且与 hostname 解耦
/// （改主机名不该让历史版本全部变成"别人写的"）。
pub type ShortId = u64;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Counter {
    pub id: ShortId,
    pub value: u64,
}

/// 版本向量。counters 恒按 id 升序——比较是双指针线性扫，不需要 map。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Vector {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub counters: Vec<Counter>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ordering {
    Equal,
    Greater,
    Lesser,
    /// 并发，且首个分歧方向是"我更大" —— 只为排序稳定，语义上就是并发
    ConcurrentGreater,
    /// 并发，且首个分歧方向是"我更小"
    ConcurrentLesser,
}

impl Ordering {
    pub fn is_concurrent(self) -> bool {
        matches!(self, Ordering::ConcurrentGreater | Ordering::ConcurrentLesser)
    }
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(1)
}

impl Vector {
    pub fn new() -> Vector {
        Vector::default()
    }

    pub fn is_empty(&self) -> bool {
        self.counters.is_empty()
    }

    pub fn counter(&self, id: ShortId) -> u64 {
        self.counters.iter().find(|c| c.id == id).map(|c| c.value).unwrap_or(0)
    }

    /// 「我改了这个文件」：把自己的计数器推进一格。
    pub fn update(&mut self, id: ShortId) {
        self.update_with_now(id, unix_secs());
    }

    pub fn update_with_now(&mut self, id: ShortId, now: u64) {
        match self.counters.binary_search_by_key(&id, |c| c.id) {
            Ok(i) => {
                let c = &mut self.counters[i];
                c.value = (c.value + 1).max(now);
            }
            Err(i) => self.counters.insert(i, Counter { id, value: now.max(1) }),
        }
    }

    /// 「我收下了对方这一版」：逐 id 取最大值。
    pub fn merge(&mut self, other: &Vector) {
        for c in &other.counters {
            match self.counters.binary_search_by_key(&c.id, |x| x.id) {
                Ok(i) => {
                    if c.value > self.counters[i].value {
                        self.counters[i].value = c.value;
                    }
                }
                Err(i) => self.counters.insert(i, *c),
            }
        }
    }

    pub fn merged(&self, other: &Vector) -> Vector {
        let mut v = self.clone();
        v.merge(other);
        v
    }

    /// 本向量相对 `other` 的关系。
    pub fn compare(&self, other: &Vector) -> Ordering {
        let (mut i, mut j) = (0usize, 0usize);
        let mut saw_greater = false;
        let mut saw_lesser = false;
        // 记住首个分歧方向，让并发有一个稳定的排序表示（syncthing 同款处理）
        let mut first_greater: Option<bool> = None;
        let mark = |greater: bool, sg: &mut bool, sl: &mut bool, first: &mut Option<bool>| {
            if greater {
                *sg = true;
            } else {
                *sl = true;
            }
            first.get_or_insert(greater);
        };

        while i < self.counters.len() || j < other.counters.len() {
            match (self.counters.get(i), other.counters.get(j)) {
                (Some(a), Some(b)) if a.id == b.id => {
                    if a.value > b.value {
                        mark(true, &mut saw_greater, &mut saw_lesser, &mut first_greater);
                    } else if a.value < b.value {
                        mark(false, &mut saw_greater, &mut saw_lesser, &mut first_greater);
                    }
                    i += 1;
                    j += 1;
                }
                // 只有我有这个 id → 我在这一维更大
                (Some(a), Some(b)) if a.id < b.id => {
                    if a.value > 0 {
                        mark(true, &mut saw_greater, &mut saw_lesser, &mut first_greater);
                    }
                    i += 1;
                }
                // 只有对方有 → 我在这一维更小
                (Some(_), Some(b)) => {
                    if b.value > 0 {
                        mark(false, &mut saw_greater, &mut saw_lesser, &mut first_greater);
                    }
                    j += 1;
                }
                (Some(a), None) => {
                    if a.value > 0 {
                        mark(true, &mut saw_greater, &mut saw_lesser, &mut first_greater);
                    }
                    i += 1;
                }
                (None, Some(b)) => {
                    if b.value > 0 {
                        mark(false, &mut saw_greater, &mut saw_lesser, &mut first_greater);
                    }
                    j += 1;
                }
                (None, None) => break,
            }
        }

        match (saw_greater, saw_lesser) {
            (false, false) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Lesser,
            (true, true) => {
                if first_greater == Some(true) {
                    Ordering::ConcurrentGreater
                } else {
                    Ordering::ConcurrentLesser
                }
            }
        }
    }

    pub fn equal(&self, other: &Vector) -> bool {
        self.compare(other) == Ordering::Equal
    }
    pub fn greater_equal(&self, other: &Vector) -> bool {
        matches!(self.compare(other), Ordering::Greater | Ordering::Equal)
    }
    pub fn lesser_equal(&self, other: &Vector) -> bool {
        matches!(self.compare(other), Ordering::Lesser | Ordering::Equal)
    }
    pub fn concurrent(&self, other: &Vector) -> bool {
        self.compare(other).is_concurrent()
    }

    /// 人类可读：`a1b2c3d4:17,e5f6:3`
    pub fn to_compact(&self) -> String {
        self.counters
            .iter()
            .map(|c| format!("{:x}:{}", c.id, c.value))
            .collect::<Vec<_>>()
            .join(",")
    }
}

// ---------- 节点身份 ----------

fn node_id_path() -> PathBuf {
    crate::config::jobs_dir()
        .parent()
        .map(|p| p.join("node-id"))
        .unwrap_or_else(|| PathBuf::from("syncdash-node-id"))
}

/// 本机稳定节点 ID。首次调用时生成并落盘；之后一直复用。
/// 生成材料 = hostname + 首次时间 + 进程 id，取 blake3 前 8 字节。
pub fn local_node_id() -> ShortId {
    let p = node_id_path();
    if let Ok(text) = std::fs::read_to_string(&p) {
        if let Ok(v) = u64::from_str_radix(text.trim(), 16) {
            if v != 0 {
                return v;
            }
        }
    }
    let seed = format!("{}|{}|{}", crate::table::host_name(), crate::foundation::time::now_ms(), std::process::id());
    let h = blake3::hash(seed.as_bytes());
    let mut b = [0u8; 8];
    b.copy_from_slice(&h.as_bytes()[..8]);
    // 0 是"缺席"的语义值，避开它
    let id = u64::from_be_bytes(b).max(1);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, format!("{id:x}\n"));
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(pairs: &[(ShortId, u64)]) -> Vector {
        let mut c: Vec<Counter> = pairs.iter().map(|&(id, value)| Counter { id, value }).collect();
        c.sort_by_key(|x| x.id);
        Vector { counters: c }
    }

    #[test]
    fn empty_vectors_are_equal() {
        assert_eq!(Vector::new().compare(&Vector::new()), Ordering::Equal);
    }

    #[test]
    fn update_makes_it_greater() {
        let base = Vector::new();
        let mut a = base.clone();
        a.update_with_now(7, 100);
        assert_eq!(a.compare(&base), Ordering::Greater);
        assert_eq!(base.compare(&a), Ordering::Lesser);
    }

    #[test]
    fn update_is_monotonic_even_with_a_stale_clock() {
        let mut a = v(&[(1, 500)]);
        a.update_with_now(1, 10); // 时钟倒退
        assert_eq!(a.counter(1), 501, "counter must never go backwards");
        let mut b = v(&[(1, 5)]);
        b.update_with_now(1, 900); // 时钟跳前
        assert_eq!(b.counter(1), 900, "wall clock is a lower bound");
    }

    #[test]
    fn independent_updates_are_concurrent() {
        let base = v(&[(1, 10)]);
        let mut a = base.clone();
        let mut b = base.clone();
        a.update_with_now(1, 11);
        b.update_with_now(2, 11);
        assert!(a.concurrent(&b), "different devices editing from a common base => conflict");
        assert!(b.concurrent(&a));
    }

    #[test]
    fn merge_resolves_concurrency() {
        let mut a = v(&[(1, 5)]);
        let b = v(&[(2, 7)]);
        assert!(a.concurrent(&b));
        a.merge(&b);
        assert_eq!(a.compare(&b), Ordering::Greater, "after merging, a dominates b");
        assert_eq!(a.counter(1), 5);
        assert_eq!(a.counter(2), 7);
    }

    #[test]
    fn descendant_is_not_a_conflict() {
        // A 改 → 传给 B → B 又改：线性历史，任何一步都不该是冲突
        let mut a = Vector::new();
        a.update_with_now(1, 100);
        let mut b = a.clone();
        b.update_with_now(2, 101);
        assert_eq!(b.compare(&a), Ordering::Greater);
        assert!(!b.concurrent(&a), "a linear history must never be reported as a conflict");
    }

    #[test]
    fn missing_counter_with_zero_value_does_not_diverge() {
        let a = v(&[(1, 3), (2, 0)]);
        let b = v(&[(1, 3)]);
        assert_eq!(a.compare(&b), Ordering::Equal, "a zero counter is the same as absent");
    }

    #[test]
    fn concurrent_variant_is_stable_for_sorting() {
        let a = v(&[(1, 9), (2, 1)]);
        let b = v(&[(1, 1), (2, 9)]);
        // 首个分歧在 id=1 上是"我更大" → ConcurrentGreater；反向必须是另一个变体
        assert_eq!(a.compare(&b), Ordering::ConcurrentGreater);
        assert_eq!(b.compare(&a), Ordering::ConcurrentLesser);
    }

    #[test]
    fn ordering_is_antisymmetric_and_reflexive() {
        // 小规模穷举，替代 property test：所有向量都与自身 Equal，
        // 且 a>b 必然 b<a、a||b 必然 b||a
        let space: Vec<Vector> = (0..3)
            .flat_map(|x| (0..3).map(move |y| v(&[(1, x), (2, y)])))
            .collect();
        for a in &space {
            assert_eq!(a.compare(a), Ordering::Equal, "reflexive");
            for b in &space {
                let ab = a.compare(b);
                let ba = b.compare(a);
                match ab {
                    Ordering::Equal => assert_eq!(ba, Ordering::Equal),
                    Ordering::Greater => assert_eq!(ba, Ordering::Lesser),
                    Ordering::Lesser => assert_eq!(ba, Ordering::Greater),
                    Ordering::ConcurrentGreater => assert_eq!(ba, Ordering::ConcurrentLesser),
                    Ordering::ConcurrentLesser => assert_eq!(ba, Ordering::ConcurrentGreater),
                }
            }
        }
    }

    #[test]
    fn merge_is_a_least_upper_bound() {
        let space: Vec<Vector> = (0..3)
            .flat_map(|x| (0..3).map(move |y| v(&[(1, x), (2, y)])))
            .collect();
        for a in &space {
            for b in &space {
                let m = a.merged(b);
                assert!(m.greater_equal(a), "merge must dominate a");
                assert!(m.greater_equal(b), "merge must dominate b");
            }
        }
    }

    #[test]
    fn compact_string_is_readable() {
        assert_eq!(v(&[(0xab, 3), (0xcd, 1)]).to_compact(), "ab:3,cd:1");
    }
}
