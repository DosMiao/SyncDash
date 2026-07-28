//! An in-memory `Vfs`, for tests only.
//!
//! Its reason to exist is `as_local() -> None`: that is what makes `scan_root` take the generic
//! `scan_vfs` lane instead of the walkdir fast path, so this is the only way the VFS lane — the
//! one every protocol backend rides — gets exercised without a live server. It also carries
//! capability knobs, which is how the degraded-evidence consent gate is testable at all.
//!
//! Deliberately `#[cfg(test)]` at its `mod` declaration and nowhere reachable from `spec::parse`:
//! a predecessor of this file was routed from a `fake://` phrase and shipped 718 lines of test
//! fixture in the release binary. A test double must not be addressable by a user.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::error::{VfsError, VfsErrorKind, VfsResult};
use super::{
    CaseSense, CommitReport, Medium, NameRules, ReadStream, Support, VDirEntry, VMeta, Vfs,
    VfsCaps, WriteHint, WriteStaged,
};
use crate::model::table::EntryKind;

#[derive(Clone, Debug)]
enum Node {
    Dir,
    File { data: Vec<u8>, mtime_ms: i64, mode: Option<u32> },
    Link { target: String, mtime_ms: i64 },
}

pub struct MemVfs {
    id: String,
    caps: VfsCaps,
    tree: Arc<Mutex<BTreeMap<String, Node>>>,
}

/// Content a given (seed, size) always generates identically, so two independent roots can hold
/// "the same file" and compare equal by hash without either one copying from the other.
pub fn filler(seed: u64, size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    let mut x = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    while out.len() < size {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(size);
    out
}

fn default_caps() -> VfsCaps {
    VfsCaps {
        protocol: "mem",
        mtime_precision_ms: 1,
        set_mtime: Support::Yes,
        fsync: Support::Yes,
        rename: Support::Yes,
        rename_overwrite: Support::Yes,
        ranged_read: Support::Yes,
        write_at: Support::Yes,
        unix_mode: Support::Yes,
        symlink: Support::Yes,
        file_id: Support::No,
        free_space: Support::Yes,
        read_back: Support::Yes,
        // It stands in for a protocol backend, so it answers as one: reached over a link, and
        // therefore not somewhere the central trash store can rename into.
        medium: Medium::NetworkShare,
        local_trash: false,
        case_sensitivity: CaseSense::Sensitive,
        name_rules: NameRules::Posix,
        max_parallel_streams: 4,
    }
}

impl MemVfs {
    pub fn new(id: &str) -> MemVfs {
        let mut tree = BTreeMap::new();
        tree.insert(String::new(), Node::Dir);
        MemVfs { id: id.to_string(), caps: default_caps(), tree: Arc::new(Mutex::new(tree)) }
    }

    /// Capability knobs, chained: `MemVfs::new("x").without(|c| c.ranged_read = Support::No)`.
    pub fn without(mut self, f: impl FnOnce(&mut VfsCaps)) -> MemVfs {
        f(&mut self.caps);
        self
    }

    pub fn seed_file(&self, rel: &str, size: usize, mtime_ms: i64) {
        self.seed_bytes(rel, &filler(size as u64, size), mtime_ms);
    }

    pub fn seed_bytes(&self, rel: &str, data: &[u8], mtime_ms: i64) {
        let mut t = self.tree.lock().unwrap();
        mkdirs(&mut t, crate::foundation::path::parent(rel));
        t.insert(rel.to_string(), Node::File { data: data.to_vec(), mtime_ms, mode: None });
    }

    fn meta_of(node: &Node) -> VMeta {
        match node {
            Node::Dir => VMeta {
                kind: EntryKind::Dir,
                size: 0,
                mtime_ms: 0,
                mode: None,
                file_id: None,
                link: None,
            },
            Node::File { data, mtime_ms, mode } => VMeta {
                kind: EntryKind::File,
                size: data.len() as u64,
                mtime_ms: *mtime_ms,
                mode: *mode,
                file_id: None,
                link: None,
            },
            Node::Link { target, mtime_ms } => VMeta {
                kind: EntryKind::Symlink,
                size: target.len() as u64,
                mtime_ms: *mtime_ms,
                mode: None,
                file_id: None,
                link: Some(target.clone()),
            },
        }
    }
}

fn mkdirs(t: &mut BTreeMap<String, Node>, rel: Option<&str>) {
    let Some(rel) = rel else { return };
    if rel.is_empty() {
        return;
    }
    let mut acc = String::new();
    for seg in rel.split('/') {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(seg);
        t.entry(acc.clone()).or_insert(Node::Dir);
    }
}

fn not_found(rel: &str) -> VfsError {
    VfsError::new(VfsErrorKind::NotFound, format!("no such entry: '{rel}'"))
}

impl Vfs for MemVfs {
    fn caps(&self) -> VfsCaps {
        self.caps.clone()
    }
    fn display(&self) -> String {
        format!("mem://{}", self.id)
    }
    fn identity(&self) -> String {
        format!("mem://{}", self.id)
    }
    fn connect(&self) -> VfsResult<()> {
        Ok(())
    }

    fn stat(&self, rel: &str) -> VfsResult<Option<VMeta>> {
        Ok(self.tree.lock().unwrap().get(rel).map(MemVfs::meta_of))
    }

    fn read_dir(&self, rel: &str) -> VfsResult<Vec<VDirEntry>> {
        let t = self.tree.lock().unwrap();
        if !t.contains_key(rel) {
            return Err(not_found(rel));
        }
        let mut out = Vec::new();
        for (k, node) in t.iter() {
            if k.is_empty() {
                continue;
            }
            let parent = crate::foundation::path::parent(k).unwrap_or("");
            if parent == rel {
                let name = k.rsplit('/').next().unwrap_or(k).to_string();
                out.push(VDirEntry { name, meta: MemVfs::meta_of(node) });
            }
        }
        Ok(out)
    }

    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>> {
        match self.tree.lock().unwrap().get(rel) {
            Some(Node::File { data, .. }) => Ok(Box::new(MemRead { data: data.clone(), pos: 0 })),
            Some(_) => Err(VfsError::new(VfsErrorKind::Io, format!("'{rel}' is not a file"))),
            None => Err(not_found(rel)),
        }
    }

    fn read_range(&self, rel: &str, off: u64, len: u32) -> VfsResult<Vec<u8>> {
        if !self.caps.ranged_read.yes() {
            return Err(VfsError::unsupported("ranged read"));
        }
        match self.tree.lock().unwrap().get(rel) {
            Some(Node::File { data, .. }) => {
                let a = (off as usize).min(data.len());
                let b = (a + len as usize).min(data.len());
                Ok(data[a..b].to_vec())
            }
            Some(_) => Err(VfsError::new(VfsErrorKind::Io, format!("'{rel}' is not a file"))),
            None => Err(not_found(rel)),
        }
    }

    fn read_link(&self, rel: &str) -> VfsResult<String> {
        match self.tree.lock().unwrap().get(rel) {
            Some(Node::Link { target, .. }) => Ok(target.clone()),
            Some(_) => Err(VfsError::new(VfsErrorKind::Io, format!("'{rel}' is not a symlink"))),
            None => Err(not_found(rel)),
        }
    }

    fn mkdir_all(&self, rel: &str) -> VfsResult<()> {
        let mut t = self.tree.lock().unwrap();
        mkdirs(&mut t, Some(rel));
        Ok(())
    }

    fn open_write(&self, rel: &str, hint: &WriteHint) -> VfsResult<Box<dyn WriteStaged>> {
        Ok(Box::new(MemStaged {
            rel: rel.to_string(),
            buf: Vec::new(),
            mtime_ms: hint.mtime_ms,
            mode: hint.mode,
            caps_read_back: self.caps.read_back,
            caps_write_at: self.caps.write_at,
            tree: self.tree.clone(),
        }))
    }

    fn rename(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        let mut t = self.tree.lock().unwrap();
        if !t.contains_key(from_rel) {
            return Err(not_found(from_rel));
        }
        if t.contains_key(to_rel) && !self.caps.rename_overwrite.yes() {
            return Err(VfsError::new(
                VfsErrorKind::AlreadyExists,
                format!("'{to_rel}' exists and this backend declares rename_overwrite=No"),
            ));
        }
        let node = t.remove(from_rel).expect("checked above");
        mkdirs(&mut t, crate::foundation::path::parent(to_rel));
        t.insert(to_rel.to_string(), node);
        Ok(())
    }

    fn remove_file(&self, rel: &str) -> VfsResult<()> {
        let mut t = self.tree.lock().unwrap();
        match t.get(rel) {
            Some(Node::Dir) => {
                Err(VfsError::new(VfsErrorKind::Io, format!("'{rel}' is a directory")))
            }
            Some(_) => {
                t.remove(rel);
                Ok(())
            }
            None => Err(not_found(rel)),
        }
    }

    fn remove_dir(&self, rel: &str) -> VfsResult<()> {
        let mut t = self.tree.lock().unwrap();
        if !matches!(t.get(rel), Some(Node::Dir)) {
            return Err(not_found(rel));
        }
        let prefix = format!("{rel}/");
        if t.keys().any(|k| k.starts_with(&prefix)) {
            return Err(VfsError::new(
                VfsErrorKind::NotEmpty,
                format!("'{rel}' is not empty"),
            ));
        }
        t.remove(rel);
        Ok(())
    }

    fn set_mtime(&self, rel: &str, ms: i64) -> VfsResult<()> {
        if !self.caps.set_mtime.yes() {
            return Err(VfsError::unsupported("set_mtime"));
        }
        let mut t = self.tree.lock().unwrap();
        match t.get_mut(rel) {
            Some(Node::File { mtime_ms, .. }) | Some(Node::Link { mtime_ms, .. }) => {
                *mtime_ms = ms;
                Ok(())
            }
            Some(Node::Dir) => Ok(()),
            None => Err(not_found(rel)),
        }
    }

    fn set_mode(&self, rel: &str, m: u32) -> VfsResult<()> {
        if !self.caps.unix_mode.yes() {
            return Err(VfsError::unsupported("unix mode"));
        }
        let mut t = self.tree.lock().unwrap();
        match t.get_mut(rel) {
            Some(Node::File { mode, .. }) => {
                *mode = Some(m);
                Ok(())
            }
            Some(_) => Ok(()),
            None => Err(not_found(rel)),
        }
    }

    fn make_symlink(&self, rel: &str, target: &str) -> VfsResult<()> {
        if !self.caps.symlink.yes() {
            return Err(VfsError::unsupported("symlink"));
        }
        let mut t = self.tree.lock().unwrap();
        mkdirs(&mut t, crate::foundation::path::parent(rel));
        t.insert(rel.to_string(), Node::Link { target: target.to_string(), mtime_ms: 0 });
        Ok(())
    }

    fn free_space(&self) -> VfsResult<Option<(u64, u64)>> {
        Ok(if self.caps.free_space.yes() { Some((1 << 40, 1 << 41)) } else { None })
    }
}

struct MemRead {
    data: Vec<u8>,
    pos: usize,
}

impl std::io::Read for MemRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (self.data.len() - self.pos).min(buf.len());
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl ReadStream for MemRead {
    fn block_size(&self) -> usize {
        64 * 1024
    }
}

/// Staging is a plain buffer: nothing lands in the tree until `commit`, which is the
/// "destination must not appear before commit / abandoned write leaves nothing" contract
/// satisfied by construction rather than by cleanup.
struct MemStaged {
    rel: String,
    buf: Vec<u8>,
    mtime_ms: Option<i64>,
    mode: Option<u32>,
    caps_read_back: Support,
    caps_write_at: Support,
    tree: Arc<Mutex<BTreeMap<String, Node>>>,
}

impl WriteStaged for MemStaged {
    fn write(&mut self, buf: &[u8]) -> VfsResult<()> {
        self.buf.extend_from_slice(buf);
        Ok(())
    }
    fn block_size(&self) -> usize {
        64 * 1024
    }
    fn write_at(&mut self, off: u64, buf: &[u8]) -> VfsResult<()> {
        if !self.caps_write_at.yes() {
            return Err(VfsError::unsupported("write_at"));
        }
        let end = off as usize + buf.len();
        if self.buf.len() < end {
            self.buf.resize(end, 0);
        }
        self.buf[off as usize..end].copy_from_slice(buf);
        Ok(())
    }
    fn seal(&mut self, _fsync: bool) -> VfsResult<()> {
        Ok(())
    }
    fn staged_len(&self) -> VfsResult<u64> {
        Ok(self.buf.len() as u64)
    }
    fn open_staged_read(&self) -> VfsResult<Box<dyn ReadStream>> {
        if !self.caps_read_back.yes() {
            return Err(VfsError::unsupported("read back"));
        }
        Ok(Box::new(MemRead { data: self.buf.clone(), pos: 0 }))
    }
    fn commit(self: Box<Self>) -> VfsResult<CommitReport> {
        let mut t = self.tree.lock().unwrap();
        mkdirs(&mut t, crate::foundation::path::parent(&self.rel));
        let mtime = self.mtime_ms.unwrap_or(0);
        t.insert(
            self.rel.clone(),
            Node::File { data: self.buf.clone(), mtime_ms: mtime, mode: self.mode },
        );
        Ok(CommitReport { mtime_ondisk_ms: Some(mtime), ..Default::default() })
    }
}
