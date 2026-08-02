//! What a root actually holds, read back through the `Vfs` surface.
//!
//! This is the assertion vocabulary for the E2E suite, and two rules are what keep it honest across
//! backends that disagree about what a filesystem even is:
//!
//! - **Content is hashed here, not read off the snapshot.** A scanner bug must not be able to make
//!   an assertion agree with itself, so `shape_of` opens every file and hashes the bytes it gets
//!   back. The scanner's opinion is the thing under test; it does not get to supply the evidence.
//! - **Nothing is asserted that the backend does not declare.** `Tolerance::between` derives from
//!   `caps()` and never from a per-lane table, so a backend that lies about its precision fails the
//!   mtime assertion — which is the correct outcome, not a false alarm to paper over.

use std::io::Read;
use std::sync::Arc;

use crate::foundation::names::{APP_DIR, LOCK_NAME, MARKER_NAME, TEMP_PREFIX, VERSION_STORE_DIR};
use crate::fs::vfs::Vfs;
use crate::fs::vfs::VfsEntryKind;

/// One entry as this harness is willing to talk about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    pub path: String,
    pub kind: VfsEntryKind,
    pub size: u64,
    /// blake3 of the content, read back through `open_read`. `None` for directories and symlinks.
    pub hash: Option<String>,
    pub mtime_ms: i64,
    pub mode: Option<u32>,
    pub link: Option<String>,
}

/// The tool's own metadata, which is never part of the tree under test. Same subject matter as
/// `foundation::names::self_excludes()`, restated here because that list is FFS filter masks and
/// this is a walker — the constants are shared, so the two cannot drift apart silently.
fn is_self_meta(name: &str) -> bool {
    name == APP_DIR
        || name == VERSION_STORE_DIR
        || name.starts_with(LOCK_NAME)
        || name == MARKER_NAME
        || name.starts_with(TEMP_PREFIX)
}

/// Walk a whole root, depth-first, sorted by path. Panics rather than returning a Result: a root
/// that cannot be read back is a broken test, not a finding about the tool.
pub fn shape_of(v: &Arc<dyn Vfs>) -> Vec<Shape> {
    let mut out = Vec::new();
    walk(v, "", &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn walk(v: &Arc<dyn Vfs>, rel: &str, out: &mut Vec<Shape>) {
    let entries = v
        .read_dir(rel)
        .unwrap_or_else(|e| panic!("read_dir({rel:?}) on {}: {e}", v.display()));
    for e in entries {
        if is_self_meta(e.name.as_str()) {
            continue;
        }
        let path = if rel.is_empty() {
            e.name.as_str().to_owned()
        } else {
            format!("{rel}/{}", e.name)
        };
        match e.meta.kind {
            VfsEntryKind::Directory => {
                out.push(Shape {
                    path: path.clone(),
                    kind: VfsEntryKind::Directory,
                    size: 0,
                    hash: None,
                    mtime_ms: e.meta.mtime_ms,
                    mode: e.meta.mode,
                    link: None,
                });
                walk(v, &path, out);
            }
            VfsEntryKind::Symlink => {
                let target = v
                    .read_link(&path)
                    .unwrap_or_else(|err| panic!("read_link({path:?}) on {}: {err}", v.display()));
                out.push(Shape {
                    path,
                    kind: VfsEntryKind::Symlink,
                    size: e.meta.size,
                    hash: None,
                    mtime_ms: e.meta.mtime_ms,
                    mode: e.meta.mode,
                    link: Some(target),
                });
            }
            VfsEntryKind::File => {
                let hash = hash_of(v, &path);
                out.push(Shape {
                    path,
                    kind: VfsEntryKind::File,
                    size: e.meta.size,
                    hash: Some(hash),
                    mtime_ms: e.meta.mtime_ms,
                    mode: e.meta.mode,
                    link: None,
                });
            }
        }
    }
}

fn hash_of(v: &Arc<dyn Vfs>, rel: &str) -> String {
    let mut buf = Vec::new();
    v.open_read(rel)
        .unwrap_or_else(|e| panic!("open_read({rel:?}) on {}: {e}", v.display()))
        .read_to_end(&mut buf)
        .unwrap_or_else(|e| panic!("read_to_end({rel:?}) on {}: {e}", v.display()));
    blake3::hash(&buf).to_hex().to_string()
}

/// What the two backends jointly permit an assertion about.
///
/// Derived from `caps()` on both sides, because an assertion is only as strong as the weaker root:
/// mode bits cannot survive a target that has none, and a timestamp cannot survive a target that
/// cannot set one.
#[derive(Clone, Copy, Debug)]
pub struct Tolerance {
    /// Storage granularity allowance, in ms. Not the 2 s compare slack — after a copy the applier
    /// explicitly sets the intended mtime, so anything beyond the coarser backend's own precision
    /// is a real failure.
    pub mtime_ms: i64,
    /// False when the target cannot set mtimes.
    pub mtime_at_all: bool,
    pub mode: bool,
    pub link: bool,
}

impl Tolerance {
    pub fn between(s: &Arc<dyn Vfs>, t: &Arc<dyn Vfs>) -> Tolerance {
        let (sc, tc) = (s.caps(), t.caps());
        Tolerance {
            mtime_ms: sc.mtime_precision_ms.max(tc.mtime_precision_ms) as i64,
            mtime_at_all: tc.set_mtime.yes(),
            mode: sc.unix_mode.yes() && tc.unix_mode.yes(),
            link: sc.symlink.yes() && tc.symlink.yes(),
        }
    }
}

/// Assert at the strongest level both backends honestly support, reporting **every** difference
/// rather than the first — one run should tell you everything that is wrong, not make you iterate.
pub fn assert_same(want: &[Shape], got: &[Shape], tol: &Tolerance, what: &str) {
    let mut diffs: Vec<String> = Vec::new();

    let want_paths: Vec<&str> = want.iter().map(|s| s.path.as_str()).collect();
    let got_paths: Vec<&str> = got.iter().map(|s| s.path.as_str()).collect();
    for p in &want_paths {
        if !got_paths.contains(p) {
            diffs.push(format!("missing: {p}"));
        }
    }
    for p in &got_paths {
        if !want_paths.contains(p) {
            diffs.push(format!("unexpected: {p}"));
        }
    }

    for w in want {
        let Some(g) = got.iter().find(|g| g.path == w.path) else {
            continue;
        };
        let at = &w.path;
        if w.kind != g.kind {
            diffs.push(format!("{at}: kind {:?} != {:?}", g.kind, w.kind));
        }
        if w.kind == VfsEntryKind::File {
            if w.size != g.size {
                diffs.push(format!("{at}: size {} != {}", g.size, w.size));
            }
            if w.hash != g.hash {
                diffs.push(format!(
                    "{at}: content differs (blake3 {} != {})",
                    short(&g.hash),
                    short(&w.hash)
                ));
            }
        }
        // Files only. A directory's mtime is a side effect of its contents, and a **symlink** is
        // compared by its target string — never by its timestamp — so requiring one to survive
        // would assert a guarantee the pipeline does not make. It could not keep it either:
        // stamping a link's own mtime needs `lutimes`, and SFTP's `setstat` follows the link, so a
        // copied link legitimately lands with the time it was created.
        if tol.mtime_at_all && w.kind == VfsEntryKind::File {
            let d = (g.mtime_ms - w.mtime_ms).abs();
            if d > tol.mtime_ms {
                diffs.push(format!(
                    "{at}: mtime off by {d} ms (tolerance {} ms)",
                    tol.mtime_ms
                ));
            }
        }
        if tol.mode && w.mode != g.mode {
            diffs.push(format!("{at}: mode {:?} != {:?}", g.mode, w.mode));
        }
        if tol.link && w.link != g.link {
            diffs.push(format!("{at}: link target {:?} != {:?}", g.link, w.link));
        }
    }

    if !diffs.is_empty() {
        panic!(
            "{what}: {} difference(s)\n  {}",
            diffs.len(),
            diffs.join("\n  ")
        );
    }
}

fn short(h: &Option<String>) -> String {
    match h {
        Some(s) => s.chars().take(12).collect(),
        None => "-".to_string(),
    }
}
