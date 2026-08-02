use std::sync::Arc;

use crate::fs::vfs::memory::filler;
use crate::fs::vfs::{Vfs, WriteHint};

#[derive(Clone, Copy, Debug)]
pub struct Seed {
    pub path: &'static str,
    pub seed: u64,
    pub size: usize,
    pub mtime_ms: i64,
}

pub const BASE: &[Seed] = &[
    Seed {
        path: "docs/readme.md",
        seed: 1,
        size: 1_024,
        mtime_ms: 1_767_225_600_000,
    },
    Seed {
        path: "code/core.rs",
        seed: 2,
        size: 4_096,
        mtime_ms: 1_767_225_601_000,
    },
];

pub fn write_seed(vfs: &Arc<dyn Vfs>, seed: Seed) {
    write_bytes(vfs, seed.path, &filler(seed.seed, seed.size), seed.mtime_ms);
}

pub fn write_bytes(vfs: &Arc<dyn Vfs>, path: &str, content: &[u8], mtime_ms: i64) {
    if let Some(parent) = crate::foundation::path::parent(path) {
        vfs.mkdir_all(parent).unwrap();
    }
    let hint = WriteHint {
        size_hint: Some(content.len() as u64),
        mtime_ms: Some(mtime_ms),
        mode: None,
    };
    let mut writer = vfs.open_write(path, &hint).unwrap();
    writer.write(content).unwrap();
    writer.seal(false).unwrap();
    writer.commit().unwrap();
    if vfs.caps().set_mtime.yes() {
        vfs.set_mtime(path, mtime_ms).unwrap();
    }
}

pub fn seed_into(vfs: &Arc<dyn Vfs>, seeds: &[Seed]) {
    for seed in seeds {
        write_seed(vfs, *seed);
    }
}
