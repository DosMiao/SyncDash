//! Junk-exclude presets: named bundles of patterns a job can paste into its own `exclude` list.
//!
//! L3 rather than in `pipeline::filter`, because a preset is job *configuration*, not matching.
//! `PathFilter::build_full` never reads them — by the time a filter exists the patterns are plain
//! lines in `Job::exclude` like any other. Every consumer is the job schema, the territory
//! generator, or a shell; the domain layer has no use for them at all.
//!
//! A preset is a **macro over `exclude`**, not a parallel rule set. Ticking one writes its patterns
//! into the list, where they appear verbatim, can be edited or deleted line by line, and survive
//! into `--exclude` on the wire to a remote scan. Unticking takes exactly those lines back out.
//! The old shape — a job saying `os_excludes = "auto"` while the exclude box said nothing — could
//! not answer "what is actually excluded here", which is the same class of lie as folding `.git`
//! in silently.

pub struct JunkPreset {
    /// Stable id: what a job file, the `--junk` CLI flag and the editor checkbox all agree on
    pub id: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub patterns: &'static [&'static str],
    /// Ticked for a brand-new job. Windows + macOS junk, matching the old `os_excludes = "auto"` default:
    /// a cross-machine sync has to guard both ends, because junk excluded on only one side is the shape
    /// that makes mirror propose deleting a tree the other side merely cannot see.
    pub default_on: bool,
}

/// The presets are **disjoint** — no pattern appears in two of them (enforced by `presets_are_disjoint`).
/// That is not cosmetic: unticking a box removes exactly its own patterns, so an overlap would let one
/// box silently strip a rule another box still needs.
///
/// The `windows`, `macos` and `dev` sets are byte-for-byte the pre-preset lists, so a job migrated off
/// `os_excludes`/`dev_excludes` excludes precisely what it excluded before — a migration must not quietly
/// widen a filter. The other four are new and ship off by default.
pub const JUNK_PRESETS: &[JunkPreset] = &[
    JunkPreset {
        id: "windows",
        label: "Windows",
        hint: "Recycle bin, restore points, the search index, per-folder view state",
        patterns: &[
            "*/System Volume Information/", "*/$RECYCLE.BIN/", "*/RECYCLE?/", "*/Recovery/",
            "*/Thumbs.db", "*/desktop.ini",
        ],
        default_on: true,
    },
    JunkPreset {
        id: "macos",
        label: "macOS",
        hint: "Spotlight/FSEvents stores, trashes, .DS_Store, and the ._ resource-fork twins a non-Mac disk collects",
        patterns: &[
            "*/.Spotlight-V100/", "*/.fseventsd/", "*/.Trashes/", "*/.TemporaryItems/",
            "*/.DocumentRevisions-V100/", "*/.DS_Store", "*/._*",
        ],
        default_on: true,
    },
    JunkPreset {
        id: "linux",
        label: "Linux",
        hint: "Per-volume trash, lost+found, KDE folder settings, and the hidden files NFS/FUSE leave behind",
        patterns: &["*/.Trash-*/", "*/lost+found/", "*/.directory", "*/.fuse_hidden*", "*/.nfs*"],
        default_on: false,
    },
    JunkPreset {
        id: "dev",
        label: "Developer",
        hint: "Rebuildable build output and dependency trees. Off by default — .git is a normal file tree too; turn it on when code travels by git and only the working files need syncing",
        patterns: &[
            "*/.git/", "*/node_modules/", "*/target/", "*/build/", "*/dist/", "*/__pycache__/",
            "*/.venv/", "*/venv/", "*/worktrees/", "*/sync.ffs_db", "*/sync.ffs_lock",
        ],
        default_on: false,
    },
    JunkPreset {
        id: "ide",
        label: "Editor / IDE state",
        hint: "Per-machine editor state: .idea, .vscode, .vs, vim swap files, editor backups. Separate from Developer on purpose — plenty of teams commit .vscode and want it synced",
        patterns: &["*/.idea/", "*/.vscode/", "*/.vs/", "*/.history/", "*/*.swp", "*/*.swo", "*/*~"],
        default_on: false,
    },
    JunkPreset {
        id: "office",
        label: "Office lock files",
        hint: "The owner/lock files Word, Excel, LibreOffice and Access write next to an open document — they change constantly, mean nothing on the far side, and make a document folder look permanently out of sync",
        patterns: &["*/~$*", "*/.~lock.*#", "*/*.laccdb", "*/*.wbk"],
        default_on: false,
    },
    JunkPreset {
        id: "synctool",
        label: "Other sync tools",
        hint: "Dropbox / Syncthing / Resilio / iCloud bookkeeping. Copying another sync tool's state across machines is a classic way to confuse it about which replica it is",
        patterns: &[
            "*/.dropbox", "*/.dropbox.attr", "*/.dropbox.cache/", "*/.stfolder/", "*/.stversions/",
            "*/.stignore", "*/.syncthing.*", "*/.sync/", "*/*.icloud",
        ],
        default_on: false,
    },
];

pub fn junk_preset(id: &str) -> Option<&'static JunkPreset> {
    JUNK_PRESETS.iter().find(|p| p.id.eq_ignore_ascii_case(id.trim()))
}

/// The exclude list a brand-new job starts with: every `default_on` preset, materialized.
/// New jobs are born with these **visible in `exclude`**, not implied by a flag somewhere.
pub fn default_junk_patterns() -> Vec<String> {
    expand_junk_presets(JUNK_PRESETS.iter().filter(|p| p.default_on).map(|p| p.id))
}

/// Preset ids → their patterns, in table order, deduplicated. Unknown ids are the caller's problem
/// (`junk_preset` returns None) — silently dropping one would be a filter that isn't what it says it is.
pub fn expand_junk_presets<I: IntoIterator<Item = S>, S: AsRef<str>>(ids: I) -> Vec<String> {
    let wanted: Vec<&str> = ids.into_iter().filter_map(|s| junk_preset(s.as_ref()).map(|p| p.id)).collect();
    let mut out = Vec::new();
    for p in JUNK_PRESETS.iter().filter(|p| wanted.contains(&p.id)) {
        for pat in p.patterns {
            if !out.iter().any(|e: &String| same_exclude_entry(e, pat)) {
                out.push((*pat).to_string());
            }
        }
    }
    out
}

/// Are these two exclude lines the same rule? Used only to keep a preset from being pasted into
/// `exclude` twice — it is a **line identity** test, not mask matching. It folds the two things the
/// filter itself folds away before a mask ever runs (separator flavour and case, via the same
/// `text::fold` the compare key uses), so `*\Thumbs.db` typed by hand counts as the entry the
/// Windows preset would add, rather than being duplicated next to it.
pub fn same_exclude_entry(a: &str, b: &str) -> bool {
    let norm = |s: &str| crate::foundation::text::fold(s.trim()).replace('\\', "/");
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::filter::PathFilter;

    /// A preset is a macro over `exclude`: expand it into the list and the filter behaves exactly as the
    /// old built-in tier did. That equivalence is what makes migrating a job file a no-op behaviourally.
    #[test]
    fn presets_expand_into_the_exclude_list() {
        let pf = PathFilter::build(&[], &default_junk_patterns());
        assert!(!pf.pass_file("a/.DS_Store"), "macOS junk, via the default-on presets");
        assert!(!pf.pass_file(".DS_Store")); // the root level is hit too, via the */x double registration
        assert!(!pf.pass_file("a/Thumbs.db"), "Windows junk, likewise");
        assert!(pf.pass_dir("proj/.git").0, "dev is not default-on");
        assert!(pf.pass_file("proj/src/main.rs"));

        // The explicit choice of a code-sync job
        let pfd = PathFilter::build(&[], &expand_junk_presets(["windows", "macos", "dev"]));
        assert!(!pfd.pass_dir("proj/.git").0);
        assert!(!pfd.pass_dir("proj/node_modules").0);
        assert!(!pfd.pass_dir("proj/node_modules").1); // no descending

        // One platform only
        let pfw = PathFilter::build(&[], &expand_junk_presets(["windows"]));
        assert!(!pfw.pass_file("a/Thumbs.db"));
        assert!(pfw.pass_file("a/.DS_Store"), "the Windows preset leaves Mac names alone");

        // The new presets
        let pfo = PathFilter::build(&[], &expand_junk_presets(["office", "ide", "linux", "synctool"]));
        assert!(!pfo.pass_file("docs/~$report.docx"), "Word owner file");
        assert!(!pfo.pass_file("docs/.~lock.report.odt#"), "LibreOffice lock file");
        assert!(!pfo.pass_dir("proj/.idea").0);
        assert!(!pfo.pass_file("src/main.rs.swp"));
        assert!(!pfo.pass_dir("vol/lost+found").0);
        assert!(!pfo.pass_dir("vol/.Trash-1000").0);
        assert!(!pfo.pass_dir("share/.stfolder").0);
        assert!(pfo.pass_file("docs/report.docx"), "the real document is untouched");
    }

    /// Unticking a preset removes exactly its own patterns, which is only sound while no pattern is
    /// shared between two presets. Enforce that here rather than discovering it as a filter that
    /// quietly lost a rule.
    #[test]
    fn presets_are_disjoint_and_well_formed() {
        let mut seen: Vec<(&str, &str)> = Vec::new();
        for p in JUNK_PRESETS {
            assert!(!p.patterns.is_empty(), "preset '{}' has no patterns", p.id);
            assert!(junk_preset(p.id).is_some(), "'{}' must resolve by id", p.id);
            for pat in p.patterns {
                if let Some((owner, _)) = seen.iter().find(|(_, q)| same_exclude_entry(q, pat)) {
                    panic!("pattern '{pat}' appears in both '{owner}' and '{}'", p.id);
                }
                seen.push((p.id, pat));
            }
        }
        // Ids are what job files and the --junk flag persist: a collision would make one unreachable
        let mut ids: Vec<&str> = JUNK_PRESETS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "preset ids must be unique");
        // An unknown id resolves to nothing — the caller decides what to do, we never guess
        assert!(junk_preset("no-such-preset").is_none());
        assert!(expand_junk_presets(["no-such-preset"]).is_empty());
    }

    /// The line-identity test that keeps a preset from being pasted in twice. It folds case and
    /// separator flavour — the same two things the filter folds before matching — and nothing else.
    #[test]
    fn duplicate_detection_folds_case_and_separators() {
        assert!(same_exclude_entry("*/Thumbs.db", "*\\thumbs.DB"));
        assert!(same_exclude_entry("  */.git/  ", "*/.git/"));
        assert!(!same_exclude_entry("*/.git/", "*/.git"), "trailing / means directory-only — a different rule");
        assert!(!same_exclude_entry("*/build/", "*/dist/"));
        // Expanding overlapping requests must not produce the same line twice
        let v = expand_junk_presets(["windows", "windows", "macos"]);
        for (i, a) in v.iter().enumerate() {
            assert!(!v[i + 1..].iter().any(|b| same_exclude_entry(a, b)), "duplicate line '{a}'");
        }
    }
}
