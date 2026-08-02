//! The trees a case starts from, and the edits that make one side drift.
//!
//! Seeding goes **through the `Vfs` write API**, exactly as `fs::vfs::conformance` does, so a
//! backend that cannot be seeded cannot be tested either — there is no side door that would let a
//! lane report a pass on a root the tool itself could not have written.
//!
//! Content comes from `memory::filler`, which is a pure function of `(seed, size)`. That is what
//! lets two independent roots hold "the same file" and compare equal by content without either one
//! copying from the other — which in turn is what gives the `(blake3, size)` move detector
//! something real to pair on rather than an artifact of how the fixture was built.
//!
//! Sizes here are not arbitrary. `pipeline::scan::digest` samples at `[0, size/2, size - 256KiB]`
//! for files at or above 4 MiB and reads anything smaller in full, and it hashes the **size** into
//! the digest prefix. So "grew by a byte" and "same size, different bytes" are genuinely different
//! code paths, and only a file over 4 MiB has a blind region at all. `SAMPLING` exists to reach it;
//! `BASE` is deliberately tiny so the other twenty cases stay fast.

use std::sync::Arc;

use crate::fs::vfs::memory::filler;
use crate::fs::vfs::{Vfs, WriteHint};

/// A file the corpus can place.
#[derive(Clone, Copy, Debug)]
pub struct Seed {
    pub path: &'static str,
    pub seed: u64,
    pub size: usize,
    pub mtime_ms: i64,
}

/// One drift applied to a root after seeding.
#[derive(Clone, Copy, Debug)]
pub enum Edit {
    /// A file that was not there before.
    Add(Seed),
    /// Same path, different content — the size changes too, so every tier can see it.
    Rewrite(Seed),
    /// Flip one byte in place, preserving size **and** mtime. The only way to construct a change
    /// that a sampled digest cannot see: pick an offset inside a blind region.
    Patch {
        path: &'static str,
        at: usize,
        xor: u8,
    },
    /// Append bytes, so the size changes but the sampling windows mostly do not.
    Grow {
        path: &'static str,
        extra: usize,
    },
    /// Same bytes, new path — what the move detector has to recognise.
    Rename {
        from: &'static str,
        to: &'static str,
    },
    Delete(&'static str),
    Touch {
        path: &'static str,
        mtime_ms: i64,
    },
    Chmod {
        path: &'static str,
        mode: u32,
    },
    Symlink {
        path: &'static str,
        target: &'static str,
    },
}

const T0: i64 = 1_767_225_600_000; // 2026-01-01T00:00:00Z — fixed, so mtime equality is controlled

/// The small tree almost every case uses. Everything here is well under the 4 MiB sampling floor,
/// so a whole lane of twenty cases costs about a megabyte of IO per root.
pub const BASE: &[Seed] = &[
    Seed {
        path: "docs/readme.md",
        seed: 1,
        size: 1_024,
        mtime_ms: T0,
    },
    Seed {
        path: "docs/manual.md",
        seed: 2,
        size: 4_096,
        mtime_ms: T0 + 1_000,
    },
    Seed {
        path: "media/thumb.png",
        seed: 3,
        size: 65_536,
        mtime_ms: T0 + 2_000,
    },
    Seed {
        path: "code/lib/core.rs",
        seed: 4,
        size: 8_192,
        mtime_ms: T0 + 3_000,
    },
    Seed {
        path: "code/lib/util.rs",
        seed: 5,
        size: 8_192,
        mtime_ms: T0 + 4_000,
    },
    Seed {
        path: "nested/a/b/c/deep.txt",
        seed: 6,
        size: 1_024,
        mtime_ms: T0 + 5_000,
    },
    Seed {
        path: "archive-me/old1.txt",
        seed: 7,
        size: 2_048,
        mtime_ms: T0 + 6_000,
    },
    Seed {
        path: "archive-me/old2.txt",
        seed: 8,
        size: 2_048,
        mtime_ms: T0 + 7_000,
    },
    // Zero-length on purpose: every empty file shares one blake3, which is why `moves.rs` refuses
    // to pair them. A case asserts that refusal rather than assuming it.
    Seed {
        path: "empty/zero-1.dat",
        seed: 0,
        size: 0,
        mtime_ms: T0 + 8_000,
    },
    // Three byte-identical files: the only way to give `detect_moves` a bucket big enough to
    // report an ambiguous pairing.
    Seed {
        path: "dupes/twin-1.bin",
        seed: 99,
        size: 4_096,
        mtime_ms: T0 + 9_000,
    },
    Seed {
        path: "dupes/twin-2.bin",
        seed: 99,
        size: 4_096,
        mtime_ms: T0 + 10_000,
    },
    Seed {
        path: "dupes/twin-3.bin",
        seed: 99,
        size: 4_096,
        mtime_ms: T0 + 11_000,
    },
];

/// The tree for the sampling cases only — the sizes that straddle the 4 MiB floor to the byte.
/// `handbook.bin` at 6 MiB has blind regions `[256K, 3M)` and `[3.25M, 5.75M)`; offset 1 MiB is in
/// the first one, which is what the blind-spot cases patch.
pub const SAMPLING: &[Seed] = &[
    Seed {
        path: "big/handbook.bin",
        seed: 20,
        size: 6 * 1_048_576,
        mtime_ms: T0,
    },
    Seed {
        path: "big/at-4mib.bin",
        seed: 21,
        size: 4 * 1_048_576,
        mtime_ms: T0 + 1_000,
    },
    Seed {
        path: "big/at-4mib-minus1.bin",
        seed: 22,
        size: 4 * 1_048_576 - 1,
        mtime_ms: T0 + 2_000,
    },
];

/// An offset inside `big/handbook.bin`'s first blind region — read by no sampling window.
pub const BLIND_OFFSET: usize = 1_048_576;
/// An offset inside the head window, which every sampling tier does read. The control.
pub const SEEN_OFFSET: usize = 10;

fn write(v: &Arc<dyn Vfs>, rel: &str, content: &[u8], mtime_ms: i64) {
    if let Some(parent) = crate::foundation::path::parent(rel) {
        v.mkdir_all(parent)
            .unwrap_or_else(|e| panic!("mkdir_all({parent:?}): {e}"));
    }
    let hint = WriteHint {
        size_hint: Some(content.len() as u64),
        mtime_ms: Some(mtime_ms),
        mode: None,
    };
    let mut w = v
        .open_write(rel, &hint)
        .unwrap_or_else(|e| panic!("open_write({rel:?}): {e}"));
    w.write(content)
        .unwrap_or_else(|e| panic!("write({rel:?}): {e}"));
    w.seal(false)
        .unwrap_or_else(|e| panic!("seal({rel:?}): {e}"));
    w.commit()
        .unwrap_or_else(|e| panic!("commit({rel:?}): {e}"));
    // The hint is advisory — some backends stamp the time on commit, some rewrite it on handle
    // close. Set it explicitly where the backend says it can, so the fixture's timestamps are a
    // fact rather than a hope.
    if v.caps().set_mtime.yes() {
        v.set_mtime(rel, mtime_ms)
            .unwrap_or_else(|e| panic!("set_mtime({rel:?}): {e}"));
    }
}

fn read(v: &Arc<dyn Vfs>, rel: &str) -> Vec<u8> {
    use std::io::Read;
    let mut buf = Vec::new();
    v.open_read(rel)
        .unwrap_or_else(|e| panic!("open_read({rel:?}): {e}"))
        .read_to_end(&mut buf)
        .unwrap_or_else(|e| panic!("read({rel:?}): {e}"));
    buf
}

/// Materialize a seed set into a root.
pub fn seed_into(v: &Arc<dyn Vfs>, seeds: &[Seed]) {
    for s in seeds {
        write(v, s.path, &filler(s.seed, s.size), s.mtime_ms);
    }
}

/// Apply the drift. Every variant goes through the same write API as seeding.
pub fn apply_edits(v: &Arc<dyn Vfs>, edits: &[Edit]) {
    for e in edits {
        match *e {
            Edit::Add(s) | Edit::Rewrite(s) => {
                write(v, s.path, &filler(s.seed, s.size), s.mtime_ms)
            }
            Edit::Patch { path, at, xor } => {
                let mut data = read(v, path);
                assert!(
                    at < data.len(),
                    "patch offset {at} is past the end of {path} ({})",
                    data.len()
                );
                let mtime = v.stat(path).unwrap().expect("patch target exists").mtime_ms;
                data[at] ^= xor;
                write(v, path, &data, mtime);
            }
            Edit::Grow { path, extra } => {
                let mut data = read(v, path);
                let mtime = v.stat(path).unwrap().expect("grow target exists").mtime_ms;
                data.extend_from_slice(&filler(7, extra));
                write(v, path, &data, mtime);
            }
            Edit::Rename { from, to } => {
                let data = read(v, from);
                let mtime = v
                    .stat(from)
                    .unwrap()
                    .expect("rename source exists")
                    .mtime_ms;
                write(v, to, &data, mtime);
                v.remove_file(from)
                    .unwrap_or_else(|e| panic!("remove_file({from:?}): {e}"));
            }
            Edit::Delete(path) => {
                v.remove_file(path)
                    .unwrap_or_else(|e| panic!("remove_file({path:?}): {e}"));
            }
            Edit::Touch { path, mtime_ms } => {
                v.set_mtime(path, mtime_ms)
                    .unwrap_or_else(|e| panic!("set_mtime({path:?}): {e}"));
            }
            Edit::Chmod { path, mode } => {
                v.set_mode(path, mode)
                    .unwrap_or_else(|e| panic!("set_mode({path:?}): {e}"));
            }
            Edit::Symlink { path, target } => {
                if let Some(parent) = crate::foundation::path::parent(path) {
                    v.mkdir_all(parent)
                        .unwrap_or_else(|e| panic!("mkdir_all({parent:?}): {e}"));
                }
                v.make_symlink(path, target)
                    .unwrap_or_else(|e| panic!("make_symlink({path:?} -> {target:?}): {e}"));
            }
        }
    }
}

/// Remove now-empty directories left behind by deletes and renames, so the fixture's own leftovers
/// are never mistaken for the tool failing to clean up. Bottom-up, and a directory that still has
/// children is simply left alone.
pub fn prune_empty_dirs(v: &Arc<dyn Vfs>, rel: &str) {
    let Ok(entries) = v.read_dir(rel) else { return };
    for e in entries {
        if e.meta.kind == crate::fs::vfs::VfsEntryKind::Directory {
            let child = if rel.is_empty() {
                e.name.as_str().to_owned()
            } else {
                format!("{rel}/{}", e.name)
            };
            prune_empty_dirs(v, &child);
        }
    }
    if !rel.is_empty() {
        if let Ok(rest) = v.read_dir(rel) {
            if rest.is_empty() {
                let _ = v.remove_dir(rel);
            }
        }
    }
}
