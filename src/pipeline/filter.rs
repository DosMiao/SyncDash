//! Path filter with FFS semantics (ported from FreeFileSync 14.10 base/path_filter.cpp, a GPL
//! project — an independent rewrite of its semantics, not copied code). Fully FFS-compatible syntax:
//!   - case-insensitive; both '/' and '\' accepted
//!   - `*` matches any run (may cross path levels), `?` matches one character (never crosses a level)
//!   - trailing `/` or `/*` = directory (and everything under it); trailing `:` = files only
//!   - leading `/` = relative to root; `*/abc` hits abc at any level and at the root alike
//!   - the include side uses "the prefix might match" (matches_begin) to decide if a dir is worth descending into
//!
//! Case folding goes through `foundation::text::fold`, the same NFC-then-uppercase the compare
//! key uses, and that is not a detail. `to_uppercase` alone left the two lanes disagreeing about
//! what a name *is*: macOS hands out NFD, a mask typed anywhere else is NFC, and the two uppercase
//! to different strings. The mask would then match on the Windows root and miss on the macOS one —
//! and an exclusion that applies to only one side is the shape that gets data deleted, because
//! the excluded side looks like it simply does not have the tree, so mirror proposes removing it
//! from the side that does. The UI meanwhile reports it as excluded on both.
//!
//! Two **superset** extensions beyond FFS syntax (in the spirit of syncthing `lib/ignore/ignore.go`;
//! the behavior of the FFS rules pasted in here is entirely unchanged):
//!   - an exclude entry starting with `!` = an **exception**: a hit lets it through, overriding every other exclude
//!     (syncthing's `!` prefix, ignore.go:372)
//!   - the `deletable` list = syncthing's `(?d)` (ignore.go:380): these do not take part in sync,
//!     but may be deleted along with their parent dir, breaking the "a filtered file sits in the dir → it can never be deleted" deadlock

use std::collections::HashSet;

#[derive(Default, Clone)]
struct Masks {
    wild: Vec<String>,
    plain: HashSet<String>, // wildcard-free relative paths: constant-time lookup (the same trick FFS uses)
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

    /// Does path match the start of some mask (with a sub-path still to come) — decides whether descending is worthwhile
    fn matches_begin(&self, path: &str) -> bool {
        self.wild.iter().any(|m| mask_begin_wild(path, m))
            || self.plain.iter().any(|m| {
                m.len() > path.len() + 1 && m.as_bytes()[path.len()] == b'/' && m.starts_with(path)
            })
    }
}

/// Byte-for-byte port of FFS matchesMask: `*` crosses '/', `?` does not
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
                        return true; // mask ends with *
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

/// path matches only the beginning of mask (FFS matchesMaskBegin<true>)
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
                    return c == b'/' && m.len() - mi > 1; // a strict sub-path is required
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
    let norm = crate::foundation::text::fold(phrase.trim()).replace('\\', "/");
    if norm.is_empty() {
        return;
    }
    fn process_tail(t: &str, set: &mut MaskSet) {
        if let Some(stripped) = t.strip_suffix(':') {
            set.file_masks.insert(stripped); // files only
        } else if t.ends_with('/') {
            set.folder_masks.insert(t.trim_end_matches('/')); // directory only (contents included)
        } else if let Some(stripped) = t.strip_suffix("/*") {
            set.folder_masks.insert(stripped);
        } else {
            set.file_masks.insert(t);
            set.folder_masks.insert(t);
        }
    }
    if let Some(tail) = norm.strip_prefix('/') {
        process_tail(tail, set); // root-relative
    } else {
        process_tail(&norm, set);
        if let Some(tail) = norm.strip_prefix("*/") {
            process_tail(tail, set); // FFS double registration: `*/abc` also hits a root-level abc
        }
    }
}

/// Exclude tiers. The lesson (in the user's own words): **truthfulness is what matters most in a sync tool** —
/// quietly folding a normal file tree like `.git` into "default excludes" turns "both sides match ✓" into a lie. Hence:
/// - `SELF_*`: this tool's own metadata, excluded unconditionally (letting locks/markers/temp files/the archive take part in sync would break its own semantics)
/// - `OS_EXCLUDES_*`: OS-level junk, per-platform presets (auto = both the Win and Mac sets, since cross-machine sync must guard both ends; can be switched off)
/// - `DEV_EXCLUDES`: rebuildable dev artifacts, **not a default** — a code-sync job opts in with `dev_excludes = true`
/// And under every tier, the number of excluded items is spelled out in the UI's "⚠ excluded"; never silently.
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
    /// `!`-prefixed exceptions: a hit lets it through, overriding exclude
    except: MaskSet,
    /// Synonym for syncthing's `(?d)`: not synced, but may be deleted along with its parent dir
    deletable: MaskSet,
    /// When an unanchored exception (one not starting with `/`) exists, excluded directories can no
    /// longer be pruned — the exception may be hiding inside. Anchored exceptions (`!/a/b/keep.log`) do not affect pruning.
    except_blocks_pruning: bool,
}

impl PathFilter {
    /// Empty include → defaults to `*`; exclude = the default list + the extra entries. The syntax
    /// is FFS syntax (plus the `!` exception prefix).
    pub fn build(includes: &[String], extra_excludes: &[String]) -> PathFilter {
        Self::build_full(includes, extra_excludes, &[])
    }

    /// Full constructor (old signature): OS junk on auto, dev artifacts **off** — `.git` is no longer a default exclude.
    pub fn build_full(includes: &[String], extra_excludes: &[String], deletables: &[String]) -> PathFilter {
        Self::build_full_opt(includes, extra_excludes, deletables, "auto", false)
    }

    /// `os_excludes`: "auto" (both the Win and Mac OS-junk presets, the default) | "windows" | "mac" | "off";
    /// `dev_excludes`: exclude rebuildable dev artifacts (.git/node_modules/…), switched on explicitly by code-sync jobs.
    /// SELF_EXCLUDES always applies.
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

    /// Does this path hit a `!` exception
    fn is_excepted(&self, path_upper: &str, parent: Option<&str>) -> bool {
        self.except.file_masks.matches(path_upper, false)
            || parent.map_or(false, |pp| self.except.folder_masks.matches(pp, true))
    }

    /// rel is '/'-separated with no leading separator
    pub fn pass_file(&self, rel: &str) -> bool {
        // Atomic-write temp files: never enter the table under any circumstances (not driven by mask matching, so editing exclude can't let them in by accident)
        if crate::fs::staged::is_temp_rel(rel) {
            return false;
        }
        let path = crate::foundation::text::fold(rel);
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

    /// Returns (does the dir itself enter the table, might a child match — decides whether to descend)
    pub fn pass_dir(&self, rel: &str) -> (bool, bool) {
        let path = crate::foundation::text::fold(rel);
        let excepted = self.except.folder_masks.matches(&path, true);
        if !excepted && self.exclude.folder_masks.matches(&path, true) {
            // Directory excluded. With an unanchored exception we still descend — the exception may be hiding inside.
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

    /// syncthing `(?d)`: this item may be removed along with its parent dir; it does not count as a "dir not empty" blocker.
    /// Temp files are inherently deletable (they are the debris of the last interrupted run).
    pub fn is_deletable(&self, rel: &str) -> bool {
        if crate::fs::staged::is_temp_rel(rel) {
            return true;
        }
        let path = crate::foundation::text::fold(rel);
        let parent = path.rfind('/').map(|i| &path[..i]);
        self.deletable.file_masks.matches(&path, false)
            || parent.map_or(false, |pp| self.deletable.folder_masks.matches(pp, true))
    }
}

/// Ad-hoc matching for the UI's "funnel": **judged by the given masks alone**, with none of the
/// built-in tiers mixed in (not one of the three SELF_/OS_/DEV_ tiers),
/// and no include whitelist applied either — it answers "did this path hit the mask the user just typed",
/// not "should this path be synced".
///
/// The decision logic is word-for-word the same as the exclude branch in `pass_file` (the very same Masks::matches),
/// so a mask tried out in the UI behaves identically once written into a job's exclude — that is the
/// entire reason this function exists: never let the frontend write a second glob implementation.
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
            let path = crate::foundation::text::fold(&rel.trim().replace('\\', "/"));
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
        // New tiers: default = SELF + OS(auto); dev artifacts (.git/node_modules) are **no longer default**
        let pf = f(&[], &[]);
        assert!(!pf.pass_file("a/.DS_Store"), "mac junk excluded by auto preset");
        assert!(!pf.pass_file(".DS_Store")); // the root level is hit too, via the */x double registration
        assert!(!pf.pass_file("a/Thumbs.db"), "windows junk excluded by auto preset");
        assert!(!pf.pass_file("x/.syncdash-root"), "SELF metadata always excluded");
        assert!(pf.pass_dir("proj/.git").0, ".git is a NORMAL tree by default now");
        assert!(pf.pass_file("proj/node_modules/x/y.js"), "dev artifacts sync by default");
        assert!(pf.pass_file("proj/src/main.rs"));
        // dev_excludes = true: the explicit choice of a code-sync job
        let pfd = PathFilter::build_full_opt(&[], &[], &[], "auto", true);
        assert!(!pfd.pass_dir("proj/.git").0);
        assert!(!pfd.pass_dir("proj/node_modules").0);
        assert!(!pfd.pass_dir("proj/node_modules").1); // no descending
        // os_excludes = "off": even OS junk passes, but SELF is still excluded unconditionally
        let pfo = PathFilter::build_full_opt(&[], &[], &[], "off", false);
        assert!(pfo.pass_file("a/Thumbs.db"));
        assert!(!pfo.pass_file("x/.syncdash.lock"), "SELF survives os=off");
        // A single platform
        let pfw = PathFilter::build_full_opt(&[], &[], &[], "windows", false);
        assert!(!pfw.pass_file("a/Thumbs.db"));
        assert!(pfw.pass_file("a/.DS_Store"), "windows preset leaves mac names alone");
    }

    #[test]
    fn include_whitelist_traversal() {
        let pf = f(&["/Research/proj/models/"], &[]);
        assert!(pf.pass_file("Research/proj/models/a.mph"));
        assert!(!pf.pass_file("Research/proj/src/a.java"));
        // Intermediate dirs don't enter the table but must be descended into
        let (pass, child) = pf.pass_dir("Research");
        assert!(!pass && child);
        let (pass, child) = pf.pass_dir("Research/proj");
        assert!(!pass && child);
        assert!(pf.pass_dir("Research/proj/models").0);
        assert!(!pf.pass_dir("Financial").1); // unrelated dirs are not descended into
    }

    #[test]
    fn wildcards_and_case() {
        let pf = f(&[], &["*/*.tmp", "*/CacheDir/"]);
        assert!(!pf.pass_file("a/b/x.TMP")); // case-insensitive
        assert!(!pf.pass_file("z/cachedir/deep/file.bin"));
        assert!(pf.pass_file("z/cachedir2/file.bin"));
    }

    /// The filter and the compare key must agree on what a name *is*. They did not: the filter
    /// case-folded without NFC, so a mask typed in NFC (anywhere but macOS) missed the NFD
    /// spelling macOS hands out. An exclusion landing on only one of two roots is the shape
    /// that deletes data — the excluded side looks like it simply lacks the tree, so mirror
    /// proposes removing it from the side that has it, while the UI calls it excluded on both.
    #[test]
    fn masks_match_across_nfc_and_nfd_spellings() {
        const NFC: &str = "Caf\u{00e9}"; // é as one code point
        const NFD: &str = "Cafe\u{0301}"; // e + combining acute
        assert_ne!(NFC, NFD, "premise: the two spellings differ byte-wise");
        assert_ne!(NFC.to_uppercase(), NFD.to_uppercase(), "premise: uppercasing alone does not reconcile them");

        // Mask written in NFC (a Windows or web UI user typing it)
        let pf = PathFilter::build(&[], &[format!("*/{NFC}/")]);
        assert!(!pf.pass_file(&format!("photos/{NFC}/a.jpg")), "the NFC spelling must be excluded");
        assert!(
            !pf.pass_file(&format!("photos/{NFD}/a.jpg")),
            "the NFD spelling macOS hands out must be excluded by the very same mask"
        );
        assert!(!pf.pass_dir(&format!("photos/{NFD}")).0, "and the directory itself");

        // ...and symmetrically, a mask typed on macOS must catch the NFC tree on Windows
        let pf2 = PathFilter::build(&[], &[format!("*/{NFD}/")]);
        assert!(!pf2.pass_file(&format!("photos/{NFC}/a.jpg")));
    }

    #[test]
    fn question_mark_no_sep() {
        let pf = f(&[], &["*/v?/"]);
        assert!(!pf.pass_file("a/v1/x"));
        assert!(pf.pass_file("a/v12/x")); // ? eats exactly one character
    }

    #[test]
    fn bang_prefix_is_an_exception() {
        let pf = f(&[], &["*/*.log", "!*/important.log"]);
        assert!(!pf.pass_file("app/debug.log"));
        assert!(pf.pass_file("app/important.log"), "! must override the exclude");
    }

    #[test]
    fn anchored_exception_keeps_directory_pruning() {
        // Anchored exceptions (leading /) don't affect pruning: excluded dirs are still not descended into (the dev tier gives us an exclude to test against)
        let pf = PathFilter::build_full_opt(&[], &["!/keep/this.txt".into()], &[], "auto", true);
        assert!(!pf.pass_dir("proj/node_modules").1, "anchored ! must not disable pruning");
        // An unanchored exception makes excluded dirs descendable again (cost traded for capability — the same trade-off gitignore makes)
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
        // Atomic-write debris is inherently deletable
        assert!(pf.is_deletable("proj/.syncdash.tmp.a.123"));
    }

    #[test]
    fn mount_marker_is_never_synced() {
        // If the marker gets synced across = an unmounted empty dir grows a marker too = the gate is defeated
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
        // Empty masks = nothing is hidden
        assert_eq!(m(&[], &["a/b.txt"]), vec![false]);
        // Extension (the shape produced by right-click "exclude this type")
        assert_eq!(m(&["*/*.log"], &["a/x.log", "x.log", "a/x.txt"]), vec![true, true, false]);
        // Anchored directory (the shape produced by right-click "exclude this directory"): hits only that one dir, not every dir with the same name
        assert_eq!(
            m(&["/a/logs/"], &["a/logs/x", "a/logs/deep/y", "b/logs/x", "a/log/x"]),
            vec![true, true, false, false]
        );
        // The default excludes must **not** be mixed in: the funnel only answers "did it hit the mask you typed"
        assert_eq!(m(&["*/*.log"], &["proj/node_modules/a.js", "proj/.git/index"]), vec![false, false]);
        // Case-insensitive, backslash equivalent
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
