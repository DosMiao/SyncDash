//! v0.5: interop with the `.ffs-sync` marker of the CodeSync ecosystem.
//! Marker semantics (see the win-mac sync architecture): a directory containing a `.ffs-sync` file = a sync territory.
//! - find_territories: scan for markers, emit territory paths relative to the root
//! - gen_jobs: generate one syncdash job per territory (cs-<slug>.toml, sync mode by default + an automatic archive path),
//!   same origin and same meaning as FFS's Update-CodeSyncConfig.ps1 — CodeSync, the syncdash edition.
//!
//! **A re-run does not overwrite a job that already exists** (pass `force` to say otherwise). It used to,
//! and that was a quiet way to lose data: the generator writes a junk-preset selection into `exclude`, so
//! a person who opened the editor and removed `*/build/` because their tree really has a `build/` folder
//! of work got that exclusion silently reinstated the next time anyone ran gen-jobs. A filter nobody
//! chose, reappearing without a word, is exactly the shape that quietly stops backing something up.
//! Generating is therefore a one-time seed per territory; after that the job file belongs to its owner.

use crate::job::Job;
use std::path::{Path, PathBuf};

const MARKER: &str = ".ffs-sync";
const PRUNE_DIRS: &[&str] = &[
    ".git", "node_modules", "target", "build", "dist", "__pycache__", ".venv", "venv",
    "worktrees", ".syncdash", "$RECYCLE.BIN", "System Volume Information",
];

/// Returns the directories holding a .ffs-sync marker (relative to root, '/'-separated, sorted)
pub fn find_territories(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .max_depth(7)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 || !e.file_type().is_dir() {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            !PRUNE_DIRS.iter().any(|d| d.eq_ignore_ascii_case(&name))
        });
    for item in walker.flatten() {
        if item.file_type().is_file() && item.file_name().to_string_lossy() == MARKER {
            if let Some(dir) = item.path().parent() {
                if let Ok(rel) = dir.strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn slug(rel: &str) -> String {
    let mut s: String = rel
        .to_lowercase()
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == ' ' || c == '.' { '-' } else { c })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    s.trim_matches('-').to_string()
}

fn archive_dir() -> PathBuf {
    crate::foundation::dirs::archive_dir()
}

pub struct GenOutcome {
    pub name: String,
    pub territory: String,
    pub path: PathBuf,
    /// false = a job of this name already existed and was left exactly as it was. Its exclude list may
    /// have been edited by hand, and the generator has no standing to overrule that.
    pub written: bool,
}

/// What the generator writes into each job. Separate from the roots so the policy knobs read as policy.
pub struct GenOpts {
    /// mirror | sync | enrich
    pub mode: String,
    pub rigor: String,
    /// Junk preset ids, materialized **in full** into each generated job's `exclude`. Never a flag that
    /// means "and also apply these": the generated TOML lists every pattern it is going to act on, and
    /// deleting a line from that list is all it takes to stop excluding it.
    pub junk: Vec<String>,
    /// Overwrite an existing cs-*.toml, discarding whatever its owner had edited into it
    pub force: bool,
    /// Where the job files land, and where sync-mode archives are pointed. The CLI passes the real
    /// config directories; tests point them at a temp dir instead of writing into someone's %APPDATA%.
    pub dest: PathBuf,
    pub archives: PathBuf,
}

impl Default for GenOpts {
    fn default() -> Self {
        GenOpts {
            mode: "sync".into(),
            rigor: "standard".into(),
            dest: crate::foundation::dirs::jobs_dir(),
            archives: archive_dir(),
            // A `.ffs-sync` marker means "this is a code tree kept in step by git", and for such a tree
            // two-way syncing `.git` is not a preference — concurrent index and ref writes from two
            // machines corrupt the repository. Hence `dev` in the seed. It is a **default**, not a
            // policy: it is spelled out in the generated file, editable there and in the UI, switchable
            // with --junk, and never reimposed on a re-run.
            junk: ["windows", "macos", "dev"].iter().map(|s| s.to_string()).collect(),
            force: false,
        }
    }
}

/// Remote pipeline generation parameters (enabled by --remote-host)
pub struct RemoteGen {
    pub host: String,
    pub root_base: String,
    pub exe: Option<String>,
}

/// Generate one `cs-<slug>.toml` job per territory. Target-side path = target_root + the same relative path.
/// Existing jobs are reported and left alone unless `opts.force`.
pub fn gen_jobs(
    source_root: &Path,
    target_root: &Path,
    opts: &GenOpts,
    remote: Option<&RemoteGen>,
) -> std::io::Result<Vec<GenOutcome>> {
    let terrs = find_territories(source_root);
    std::fs::create_dir_all(&opts.dest)?;
    std::fs::create_dir_all(&opts.archives)?;
    let junk = crate::job::junk::expand_junk_presets(&opts.junk);
    let mut out = Vec::new();
    for rel in terrs {
        let name = format!("cs-{}", slug(&rel));
        let path = opts.dest.join(format!("{name}.toml"));
        if path.exists() && !opts.force {
            out.push(GenOutcome { name, territory: rel, path, written: false });
            continue;
        }
        let native = crate::foundation::path::to_native(&rel);
        // target_root may be UNC or posix — join each side with its own separator
        let mount = if target_root.to_string_lossy().contains('/') && !target_root.to_string_lossy().contains('\\') {
            format!("{}/{}", target_root.to_string_lossy().trim_end_matches('/'), rel)
        } else {
            target_root.join(&native).to_string_lossy().into_owned()
        };
        // With a peer, the mount is no longer the target — it is the path the *pull* direction
        // writes through, and it rides on the phrase where the rest of the link already lives.
        let target = match remote {
            Some(r) => {
                let mut p = format!("peer://{}/{}/{}", r.host, r.root_base.trim_end_matches('/').trim_start_matches('/'), rel);
                if let Some(exe) = r.exe.as_deref().filter(|e| !e.trim().is_empty()) {
                    p.push_str(&format!("|exe={exe}"));
                }
                p.push_str(&format!("|mount={mount}"));
                p
            }
            None => mount,
        };
        let job = Job {
            mode: opts.mode.clone(),
            source: source_root.join(&native).to_string_lossy().into_owned(),
            target,
            archive: if opts.mode == "sync" { Some(opts.archives.join(format!("{name}.jsonl"))) } else { None },
            include: Vec::new(),
            // The seed selection, materialized in full. Every rule this job will act on is a line here —
            // there is no companion flag that adds more behind it, and no re-run that puts back a line
            // its owner deleted.
            exclude: junk.clone(),
            no_hash: false,
            rigor: opts.rigor.clone(),
            case_sensitive: false,
            symlinks: "exclude".into(),
            versioning: false,
            ..Default::default()
        };
        let toml_text = toml::to_string_pretty(&job)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("toml serialize: {e}")))?;
        let seed = if opts.junk.is_empty() { "none".to_string() } else { opts.junk.join(", ") };
        let header = format!(
            "# generated by `syncdash gen-jobs` from the .ffs-sync marker: {rel}\n\
             # Seeded once with the junk presets: {seed} ({} patterns, all listed under `exclude` below).\n\
             # `exclude` is the whole filter — nothing else is applied on top of it, so deleting a line here\n\
             # is all it takes to stop excluding it. A re-run of gen-jobs will NOT overwrite this file\n\
             # (use --force to discard your edits and reseed).\n",
            junk.len(),
        );
        std::fs::write(&path, format!("{header}{toml_text}"))?;
        out.push(GenOutcome { name, territory: rel, path, written: true });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("syncdash-terr-{tag}-{}", crate::foundation::time::now_ms()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn opts(root: &Path, junk: &[&str], force: bool) -> GenOpts {
        GenOpts {
            mode: "mirror".into(),
            junk: junk.iter().map(|s| s.to_string()).collect(),
            force,
            dest: root.join("jobs"),
            archives: root.join("archives"),
            ..Default::default()
        }
    }

    /// A territory tree with one marked directory
    fn territory_tree(root: &Path) -> PathBuf {
        let src = root.join("src");
        std::fs::create_dir_all(src.join("proj")).unwrap();
        std::fs::write(src.join("proj").join(MARKER), "").unwrap();
        src
    }

    /// The seed is written out as ordinary exclude lines, not implied by a flag: everything the
    /// generated job will filter is readable in the file it just wrote.
    #[test]
    fn the_seed_is_visible_in_the_generated_file() {
        let root = tmp("seed");
        let src = territory_tree(&root);
        let o = gen_jobs(&src, Path::new(r"T:\dst"), &opts(&root, &["windows", "dev"], false), None).unwrap();
        assert_eq!(o.len(), 1);
        assert!(o[0].written);

        let (_, job) = crate::job::load(&o[0].path.to_string_lossy()).unwrap();
        let expect = crate::job::junk::expand_junk_presets(["windows", "dev"]);
        assert_eq!(job.exclude, expect, "every rule is a line in exclude");
        // …and the same lines are literally in the text, not hidden behind a preset name
        let text = std::fs::read_to_string(&o[0].path).unwrap();
        for p in &expect {
            assert!(text.contains(p.as_str()), "pattern '{p}' must appear verbatim in the job file");
        }
        assert!(!text.contains("dev_excludes"), "no flag may stand in for the list");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The regression this exists to prevent: someone opens a generated job, removes an exclude line
    /// because their tree really does have a `build/` folder of work, and a later gen-jobs run puts it
    /// back without a word — so that data silently stops being copied.
    #[test]
    fn a_rerun_never_resurrects_an_exclude_its_owner_deleted() {
        let root = tmp("keep");
        let src = territory_tree(&root);
        let o1 = gen_jobs(&src, Path::new(r"T:\dst"), &opts(&root, &["windows", "dev"], false), None).unwrap();
        let path = o1[0].path.clone();
        assert!(crate::job::load(&path.to_string_lossy()).unwrap().1.exclude.iter().any(|e| e == "*/build/"));

        // The owner edits the job: */build/ is theirs to keep
        let (_, mut edited) = crate::job::load(&path.to_string_lossy()).unwrap();
        edited.exclude.retain(|e| e != "*/build/");
        std::fs::write(&path, toml::to_string_pretty(&edited).unwrap()).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let o2 = gen_jobs(&src, Path::new(r"T:\dst"), &opts(&root, &["windows", "dev"], false), None).unwrap();
        assert!(!o2[0].written, "an existing job must be reported as kept, not rewritten");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "the file must be byte-identical");
        let after = crate::job::load(&path.to_string_lossy()).unwrap().1;
        assert!(!after.exclude.iter().any(|e| e == "*/build/"), "a deleted exclude stays deleted");

        // --force is the explicit way back, and only then
        let o3 = gen_jobs(&src, Path::new(r"T:\dst"), &opts(&root, &["windows", "dev"], true), None).unwrap();
        assert!(o3[0].written);
        assert!(crate::job::load(&path.to_string_lossy()).unwrap().1.exclude.iter().any(|e| e == "*/build/"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `--junk none` really means none: the generator has no floor it silently applies underneath.
    #[test]
    fn seeding_nothing_seeds_nothing() {
        let root = tmp("none");
        let src = territory_tree(&root);
        let o = gen_jobs(&src, Path::new(r"T:\dst"), &opts(&root, &[], false), None).unwrap();
        assert!(crate::job::load(&o[0].path.to_string_lossy()).unwrap().1.exclude.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
