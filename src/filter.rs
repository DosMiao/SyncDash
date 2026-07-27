//! FFS 语义的路径过滤器（移植自 FreeFileSync 14.10 base/path_filter.cpp，GPL 项目——
//! 这里是按其语义的独立重写，不是抄代码）。语法与 FFS 完全兼容：
//!   - 大小写不敏感；'/' 与 '\' 均可
//!   - `*` 匹配任意串（可跨路径层级），`?` 匹配单字符（不跨层级）
//!   - 尾部 `/` 或 `/*` = 目录（含其下全部内容）；尾部 `:` = 仅文件
//!   - 开头 `/` = 相对 root；`*/abc` 同时命中任意层级与根级的 abc
//!   - include 侧用"前缀可能命中"（matches_begin）决定目录是否值得下钻
//!
//! FFS 语法之外的两处**超集**扩展（借 syncthing `lib/ignore/ignore.go` 之意，
//! 粘进来的 FFS 规则行为完全不变）：
//!   - exclude 项以 `!` 开头 = **例外**：命中即放行，压过其它 exclude
//!     （syncthing 的 `!` 前缀，ignore.go:372）
//!   - `deletable` 列表 = syncthing 的 `(?d)`（ignore.go:380）：这些东西不参与同步，
//!     但删除父目录时允许连带删掉，解开"目录里有被过滤的文件 → 永远删不掉"的死结

use std::collections::HashSet;

#[derive(Default, Clone)]
struct Masks {
    wild: Vec<String>,
    plain: HashSet<String>, // 无通配符的相对路径：常数时间查找（FFS 同款优化）
}

impl Masks {
    fn insert(&mut self, mask: &str) {
        if mask.is_empty() {
            return;
        }
        if mask.contains('?') || mask.contains('*') {
            self.wild.push(mask.to_string());
        } else {
            self.plain.insert(mask.to_string());
        }
    }

    fn matches(&self, path: &str, allow_parent: bool) -> bool {
        if self.wild.iter().any(|m| matches_mask(path, m, allow_parent)) {
            return true;
        }
        if allow_parent {
            let mut p = path;
            loop {
                if self.plain.contains(p) {
                    return true;
                }
                match p.rfind('/') {
                    Some(i) => p = &p[..i],
                    None => return false,
                }
            }
        } else {
            self.plain.contains(path)
        }
    }

    /// path 是否命中某个 mask 的开头（还差子路径）—— 用于决定是否值得继续下钻
    fn matches_begin(&self, path: &str) -> bool {
        self.wild.iter().any(|m| mask_begin_wild(path, m))
            || self.plain.iter().any(|m| {
                m.len() > path.len() + 1 && m.as_bytes()[path.len()] == b'/' && m.starts_with(path)
            })
    }
}

/// FFS matchesMask 的逐字节移植：`*` 跨 '/'，`?` 不跨
fn matches_mask(path: &str, mask: &str, allow_parent: bool) -> bool {
    fn go(p: &[u8], m: &[u8], allow_parent: bool) -> bool {
        let mut pi = 0usize;
        let mut mi = 0usize;
        loop {
            if mi == m.len() {
                return if allow_parent { pi == p.len() || p[pi] == b'/' } else { pi == p.len() };
            }
            match m[mi] {
                b'?' => {
                    if pi == p.len() || p[pi] == b'/' {
                        return false;
                    }
                    pi += 1;
                    mi += 1;
                }
                b'*' => {
                    while mi < m.len() && m[mi] == b'*' {
                        mi += 1;
                    }
                    if mi == m.len() {
                        return true; // mask 以 * 结尾
                    }
                    let c = m[mi];
                    mi += 1;
                    if c == b'?' {
                        while pi < p.len() {
                            let ch = p[pi];
                            pi += 1;
                            if ch != b'/' && go(&p[pi..], &m[mi..], allow_parent) {
                                return true;
                            }
                        }
                    } else {
                        while pi < p.len() {
                            let ch = p[pi];
                            pi += 1;
                            if ch == c && go(&p[pi..], &m[mi..], allow_parent) {
                                return true;
                            }
                        }
                    }
                    return false;
                }
                c => {
                    if pi == p.len() || p[pi] != c {
                        return false;
                    }
                    pi += 1;
                    mi += 1;
                }
            }
        }
    }
    go(path.as_bytes(), mask.as_bytes(), allow_parent)
}

/// path 只匹配 mask 的开头（FFS matchesMaskBegin<true>）
fn mask_begin_wild(path: &str, mask: &str) -> bool {
    let p = path.as_bytes();
    let m = mask.as_bytes();
    let mut pi = 0usize;
    for mi in 0..m.len() {
        match m[mi] {
            b'?' => {
                if pi >= p.len() || p[pi] == b'/' {
                    return false;
                }
                pi += 1;
            }
            b'*' => return true,
            c => {
                if pi >= p.len() {
                    return c == b'/' && m.len() - mi > 1; // 要求严格子路径
                }
                if p[pi] != c {
                    return false;
                }
                pi += 1;
            }
        }
    }
    false
}

#[derive(Default, Clone)]
struct MaskSet {
    file_masks: Masks,
    folder_masks: Masks,
}

impl MaskSet {
    fn is_empty(&self) -> bool {
        self.file_masks.wild.is_empty()
            && self.file_masks.plain.is_empty()
            && self.folder_masks.wild.is_empty()
            && self.folder_masks.plain.is_empty()
    }
}

fn parse_phrase(phrase: &str, set: &mut MaskSet) {
    let norm = phrase.trim().to_uppercase().replace('\\', "/");
    if norm.is_empty() {
        return;
    }
    fn process_tail(t: &str, set: &mut MaskSet) {
        if let Some(stripped) = t.strip_suffix(':') {
            set.file_masks.insert(stripped); // 仅文件
        } else if t.ends_with('/') {
            set.folder_masks.insert(t.trim_end_matches('/')); // 仅目录（含内容）
        } else if let Some(stripped) = t.strip_suffix("/*") {
            set.folder_masks.insert(stripped);
        } else {
            set.file_masks.insert(t);
            set.folder_masks.insert(t);
        }
    }
    if let Some(tail) = norm.strip_prefix('/') {
        process_tail(tail, set); // 根相对
    } else {
        process_tail(&norm, set);
        if let Some(tail) = norm.strip_prefix("*/") {
            process_tail(tail, set); // FFS 双注册：`*/abc` 也命中根级 abc
        }
    }
}

/// 排除分层。教训（用户原话级）：**同步工具最重要的是真实性**——把 `.git` 这种正常
/// 文件树塞进"默认排除"并静默生效，会让"两侧一致 ✓"变成谎言。因此：
/// - `SELF_*`：本工具自身元数据，无条件排除（锁/标记/临时件/版本库参与同步会破坏自身语义）
/// - `OS_EXCLUDES_*`：系统级垃圾，按平台预设（auto = Win+Mac 两份都上，跨机同步两边都要防；可关）
/// - `DEV_EXCLUDES`：可重建的开发产物，**不是默认**——代码同步任务显式 `dev_excludes = true`
/// 且无论哪档，被排除的数量都在界面"⚠ 已排除"里明示，绝不静默。
pub const OS_EXCLUDES_WINDOWS: &[&str] = &[
    "*/System Volume Information/", "*/$RECYCLE.BIN/", "*/RECYCLE?/", "*/Recovery/",
    "*/Thumbs.db", "*/desktop.ini",
];

pub const OS_EXCLUDES_MAC: &[&str] = &[
    "*/.Spotlight-V100/", "*/.fseventsd/", "*/.Trashes/", "*/.TemporaryItems/",
    "*/.DocumentRevisions-V100/", "*/.DS_Store", "*/._*",
];

pub const DEV_EXCLUDES: &[&str] = &[
    "*/.git/", "*/node_modules/", "*/target/", "*/build/", "*/dist/", "*/__pycache__/",
    "*/.venv/", "*/venv/", "*/worktrees/", "*/sync.ffs_db", "*/sync.ffs_lock",
];

#[derive(Clone)]
pub struct PathFilter {
    include: MaskSet,
    exclude: MaskSet,
    /// `!` 前缀的例外：命中即放行，压过 exclude
    except: MaskSet,
    /// syncthing `(?d)` 同义：不同步，但删父目录时可连带删
    deletable: MaskSet,
    /// 存在非锚定（不以 `/` 开头）的例外时，被排除的目录不能再剪枝——
    /// 因为例外可能藏在里面。锚定例外（`!/a/b/keep.log`）不影响剪枝。
    except_blocks_pruning: bool,
}

impl PathFilter {
    /// include 为空 → 默认 `*`；exclude = 默认列表 + 追加项。语法即 FFS 语法
    /// （外加 `!` 例外前缀）。
    pub fn build(includes: &[String], extra_excludes: &[String]) -> PathFilter {
        Self::build_full(includes, extra_excludes, &[])
    }

    /// 完整构造（旧签名）：系统垃圾 auto、开发产物**关**——`.git` 不再是默认排除。
    pub fn build_full(includes: &[String], extra_excludes: &[String], deletables: &[String]) -> PathFilter {
        Self::build_full_opt(includes, extra_excludes, deletables, "auto", false)
    }

    /// `os_excludes`: "auto"（Win+Mac 两份系统垃圾预设，默认）| "windows" | "mac" | "off"；
    /// `dev_excludes`: 排除可重建开发产物（.git/node_modules/…），代码同步任务显式开。
    /// SELF_EXCLUDES 无条件生效。
    pub fn build_full_opt(
        includes: &[String],
        extra_excludes: &[String],
        deletables: &[String],
        os_excludes: &str,
        dev_excludes: bool,
    ) -> PathFilter {
        let mut inc = MaskSet::default();
        if includes.is_empty() {
            parse_phrase("*", &mut inc);
        } else {
            for p in includes {
                parse_phrase(p, &mut inc);
            }
        }
        let mut exc = MaskSet::default();
        let mut exn = MaskSet::default();
        let mut blocks_pruning = false;
        for p in crate::foundation::names::self_excludes() {
            parse_phrase(&p, &mut exc);
        }
        match os_excludes {
            "off" => {}
            "windows" => {
                for p in OS_EXCLUDES_WINDOWS {
                    parse_phrase(p, &mut exc);
                }
            }
            "mac" => {
                for p in OS_EXCLUDES_MAC {
                    parse_phrase(p, &mut exc);
                }
            }
            _ => {
                for p in OS_EXCLUDES_WINDOWS.iter().chain(OS_EXCLUDES_MAC) {
                    parse_phrase(p, &mut exc);
                }
            }
        }
        if dev_excludes {
            for p in DEV_EXCLUDES {
                parse_phrase(p, &mut exc);
            }
        }
        for p in extra_excludes {
            match p.trim().strip_prefix('!') {
                Some(rest) => {
                    let rest = rest.trim();
                    if rest.is_empty() {
                        continue;
                    }
                    if !rest.starts_with('/') {
                        blocks_pruning = true;
                    }
                    parse_phrase(rest, &mut exn);
                }
                None => parse_phrase(p, &mut exc),
            }
        }
        let mut del = MaskSet::default();
        for p in deletables {
            parse_phrase(p, &mut del);
        }
        PathFilter { include: inc, exclude: exc, except: exn, deletable: del, except_blocks_pruning: blocks_pruning }
    }

    /// 该路径是否命中 `!` 例外
    fn is_excepted(&self, path_upper: &str, parent: Option<&str>) -> bool {
        self.except.file_masks.matches(path_upper, false)
            || parent.map_or(false, |pp| self.except.folder_masks.matches(pp, true))
    }

    /// rel 用 '/' 分隔、不带开头分隔符
    pub fn pass_file(&self, rel: &str) -> bool {
        // 原子写的临时文件：任何情况下都不入表（不依赖模式匹配，避免用户改 exclude 时误开）
        if crate::atomic::is_temp_rel(rel) {
            return false;
        }
        let path = rel.to_uppercase();
        let parent = path.rfind('/').map(|i| &path[..i]);
        if !self.is_excepted(&path, parent)
            && (self.exclude.file_masks.matches(&path, false)
                || parent.map_or(false, |pp| self.exclude.folder_masks.matches(pp, true)))
        {
            return false;
        }
        self.include.file_masks.matches(&path, false)
            || parent.map_or(false, |pp| self.include.folder_masks.matches(pp, true))
    }

    /// 返回 (目录本身是否入表, 子项是否可能命中——决定要不要下钻)
    pub fn pass_dir(&self, rel: &str) -> (bool, bool) {
        let path = rel.to_uppercase();
        let excepted = self.except.folder_masks.matches(&path, true);
        if !excepted && self.exclude.folder_masks.matches(&path, true) {
            // 目录被排除。有非锚定例外时仍要下钻——例外可能就藏在里面。
            let child = self.except_blocks_pruning
                && (self.except.file_masks.matches_begin(&path)
                    || self.except.folder_masks.matches_begin(&path));
            return (false, child);
        }
        if self.include.folder_masks.matches(&path, true) {
            return (true, true);
        }
        let child = self.include.file_masks.matches_begin(&path)
            || self.include.folder_masks.matches_begin(&path);
        (false, child)
    }

    /// syncthing `(?d)`：删除父目录时这一项可以连带删掉，不算"目录非空"的阻塞物。
    /// 临时文件天然可删（它们就是上次中断的残骸）。
    pub fn is_deletable(&self, rel: &str) -> bool {
        if crate::atomic::is_temp_rel(rel) {
            return true;
        }
        let path = rel.to_uppercase();
        let parent = path.rfind('/').map(|i| &path[..i]);
        self.deletable.file_masks.matches(&path, false)
            || parent.map_or(false, |pp| self.deletable.folder_masks.matches(pp, true))
    }
}

/// 界面"漏斗"用的即席匹配：**只按给定的掩码判定**，不掺任何内置档位
/// （SELF_/OS_/DEV_ 三档一个都不加）、
/// 也不套 include 白名单——它回答的是"这条路径命中用户此刻输入的掩码了吗"，
/// 不是"这条路径该不该同步"。
///
/// 判定逻辑与 `pass_file` 里的 exclude 分支逐字同源（同一个 Masks::matches），
/// 所以界面里试出来的掩码，写进任务的 exclude 后行为一致——这是这个函数存在的
/// 全部理由：绝不让前端再写一份 glob。
pub fn mask_hits(masks: &[String], rels: &[String]) -> Vec<bool> {
    let mut set = MaskSet::default();
    for m in masks {
        if !m.trim().is_empty() {
            parse_phrase(m, &mut set);
        }
    }
    if set.is_empty() {
        return vec![false; rels.len()];
    }
    rels.iter()
        .map(|rel| {
            let path = rel.trim().replace('\\', "/").to_uppercase();
            let parent = path.rfind('/').map(|i| path[..i].to_string());
            set.file_masks.matches(&path, false)
                || parent.as_deref().map_or(false, |pp| set.folder_masks.matches(pp, true))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn f(inc: &[&str], exc: &[&str]) -> PathFilter {
        PathFilter::build(
            &inc.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &exc.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }

    #[test]
    fn default_excludes_work() {
        // 新分层：默认 = SELF + OS(auto)；开发产物（.git/node_modules）**不再是默认**
        let pf = f(&[], &[]);
        assert!(!pf.pass_file("a/.DS_Store"), "mac junk excluded by auto preset");
        assert!(!pf.pass_file(".DS_Store")); // 根级也被 */x 双注册命中
        assert!(!pf.pass_file("a/Thumbs.db"), "windows junk excluded by auto preset");
        assert!(!pf.pass_file("x/.syncdash-root"), "SELF metadata always excluded");
        assert!(pf.pass_dir("proj/.git").0, ".git is a NORMAL tree by default now");
        assert!(pf.pass_file("proj/node_modules/x/y.js"), "dev artifacts sync by default");
        assert!(pf.pass_file("proj/src/main.rs"));
        // dev_excludes = true：代码同步任务的显式选择
        let pfd = PathFilter::build_full_opt(&[], &[], &[], "auto", true);
        assert!(!pfd.pass_dir("proj/.git").0);
        assert!(!pfd.pass_dir("proj/node_modules").0);
        assert!(!pfd.pass_dir("proj/node_modules").1); // 不下钻
        // os_excludes = "off"：连系统垃圾都放行，但 SELF 仍然无条件排除
        let pfo = PathFilter::build_full_opt(&[], &[], &[], "off", false);
        assert!(pfo.pass_file("a/Thumbs.db"));
        assert!(!pfo.pass_file("x/.syncdash.lock"), "SELF survives os=off");
        // 平台单选
        let pfw = PathFilter::build_full_opt(&[], &[], &[], "windows", false);
        assert!(!pfw.pass_file("a/Thumbs.db"));
        assert!(pfw.pass_file("a/.DS_Store"), "windows preset leaves mac names alone");
    }

    #[test]
    fn include_whitelist_traversal() {
        let pf = f(&["/Research/proj/models/"], &[]);
        assert!(pf.pass_file("Research/proj/models/a.mph"));
        assert!(!pf.pass_file("Research/proj/src/a.java"));
        // 中间目录不入表但要下钻
        let (pass, child) = pf.pass_dir("Research");
        assert!(!pass && child);
        let (pass, child) = pf.pass_dir("Research/proj");
        assert!(!pass && child);
        assert!(pf.pass_dir("Research/proj/models").0);
        assert!(!pf.pass_dir("Financial").1); // 无关目录不下钻
    }

    #[test]
    fn wildcards_and_case() {
        let pf = f(&[], &["*/*.tmp", "*/CacheDir/"]);
        assert!(!pf.pass_file("a/b/x.TMP")); // 大小写不敏感
        assert!(!pf.pass_file("z/cachedir/deep/file.bin"));
        assert!(pf.pass_file("z/cachedir2/file.bin"));
    }

    #[test]
    fn question_mark_no_sep() {
        let pf = f(&[], &["*/v?/"]);
        assert!(!pf.pass_file("a/v1/x"));
        assert!(pf.pass_file("a/v12/x")); // ? 只吃一个字符
    }

    #[test]
    fn bang_prefix_is_an_exception() {
        let pf = f(&[], &["*/*.log", "!*/important.log"]);
        assert!(!pf.pass_file("app/debug.log"));
        assert!(pf.pass_file("app/important.log"), "! must override the exclude");
    }

    #[test]
    fn anchored_exception_keeps_directory_pruning() {
        // 锚定例外（以 / 开头）不影响剪枝：被排除目录依然不下钻（用 dev 档拿到排除项）
        let pf = PathFilter::build_full_opt(&[], &["!/keep/this.txt".into()], &[], "auto", true);
        assert!(!pf.pass_dir("proj/node_modules").1, "anchored ! must not disable pruning");
        // 非锚定例外会让被排除目录重新可下钻（代价换能力，与 gitignore 同样的取舍）
        let pf2 = PathFilter::build_full_opt(&[], &["!*/keep.txt".into()], &[], "auto", true);
        assert!(pf2.pass_dir("proj/node_modules").1, "unanchored ! must allow descending");
    }

    #[test]
    fn exception_survives_an_excluded_parent_dir() {
        let pf = f(&[], &["*/logs/", "!*/logs/audit.txt"]);
        assert!(!pf.pass_file("srv/logs/debug.txt"));
        assert!(pf.pass_file("srv/logs/audit.txt"), "! must beat an excluded parent dir");
        assert!(pf.pass_dir("srv/logs").1, "must descend to reach the exception");
    }

    #[test]
    fn deletable_list_and_temp_files() {
        let pf = PathFilter::build_full(&[], &[], &["*/node_modules/".to_string()]);
        assert!(pf.is_deletable("proj/node_modules/x/y.js"));
        assert!(!pf.is_deletable("proj/src/main.rs"));
        // 原子写的残骸天然可删
        assert!(pf.is_deletable("proj/.syncdash.tmp.a.123"));
    }

    #[test]
    fn mount_marker_is_never_synced() {
        // 标记被同步过去 = 没挂载的空目录也会长出标记 = 闸门失效
        let pf = f(&[], &[]);
        assert!(!pf.pass_file(".syncdash-root"));
        assert!(!pf.pass_file("nested/.syncdash-root"));
    }

    #[test]
    fn mask_hits_matches_only_the_given_masks() {
        let m = |v: &[&str], p: &[&str]| {
            mask_hits(
                &v.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                &p.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            )
        };
        // 空掩码 = 什么都不隐藏
        assert_eq!(m(&[], &["a/b.txt"]), vec![false]);
        // 扩展名（右键"排除此类型"生成的形状）
        assert_eq!(m(&["*/*.log"], &["a/x.log", "x.log", "a/x.txt"]), vec![true, true, false]);
        // 锚定目录（右键"排除此目录"生成的形状）：只命中那一个目录，不是同名的所有目录
        assert_eq!(
            m(&["/a/logs/"], &["a/logs/x", "a/logs/deep/y", "b/logs/x", "a/log/x"]),
            vec![true, true, false, false]
        );
        // 默认排除**不该**混进来：漏斗只回答"命中你输入的掩码了吗"
        assert_eq!(m(&["*/*.log"], &["proj/node_modules/a.js", "proj/.git/index"]), vec![false, false]);
        // 大小写不敏感、反斜杠等价
        assert_eq!(m(&["*/CacheDir/"], &["z\\cachedir\\deep\\f.bin"]), vec![true]);
    }

    #[test]
    fn temp_files_never_enter_the_table() {
        let pf = f(&[], &[]);
        assert!(!pf.pass_file(".syncdash.tmp.report.pdf.4242"));
        assert!(!pf.pass_file("deep/dir/.syncdash.tmp.x.1"));
        assert!(pf.pass_file("deep/dir/x"));
    }
}
