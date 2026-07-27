//! The in-memory test filesystem — a first-class `Vfs`, not a mock (syncthing's
//! fakeFS approach). Two properties make it useful:
//!
//! 1. **Content is a function of (seed, path, offset)**, generated through a blake3
//!    XOF: two instances with the same seed agree on every byte, so full-transfer
//!    correctness is testable in O(metadata) memory. Files *written* through the
//!    API store their real bytes instead.
//! 2. **Pathologies are knobs in the phrase**, so a test names its world in one line:
//!
//!    `fake://seed?latency_ms=5&precision_ms=60000&insens&no_ranged_read&no_set_mtime`
//!    `&no_fsync&no_write_at&no_symlink&no_unix_mode&no_free_space&no_read_back`
//!    `&no_rename_overwrite&streams=2&block_size=65536&transient_after=40&fail_read=big.bin`
//!
//! Built-in op counters turn performance regressions into plain asserts
//! ("a cache-hit rescan does zero open_read", "a pruned subtree does zero read_dir").

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::error::{VfsError, VfsErrorKind, VfsResult};
use super::{
    CaseSense, CommitReport, EntryKind, ReadStream, Support, VDirEntry, VMeta, Vfs, VfsCaps,
    WriteHint, WriteStaged,
};

#[derive(Clone)]
struct FNode {
    kind: EntryKind,
    size: u64,
    mtime_ms: i64,
    mode: Option<u32>,
    link: Option<String>,
    /// None = deterministic generated content; Some = bytes written through the API.
    content: Option<Arc<Vec<u8>>>,
}

#[derive(Default)]
pub struct FakeCounters {
    pub stat: AtomicU64,
    pub read_dir: AtomicU64,
    pub open_read: AtomicU64,
    pub read_range: AtomicU64,
    pub open_write: AtomicU64,
    pub bytes_read: AtomicU64,
    pub bytes_written: AtomicU64,
}

#[derive(Clone, Debug)]
struct Knobs {
    latency_ms: u64,
    precision_ms: u32,
    insens: bool,
    no_ranged_read: bool,
    no_set_mtime: bool,
    no_fsync: bool,
    no_write_at: bool,
    no_symlink: bool,
    no_unix_mode: bool,
    no_free_space: bool,
    no_read_back: bool,
    no_rename_overwrite: bool,
    streams: usize,
    block_size: usize,
    transient_after: Option<u64>,
    fail_read: Option<String>,
}

impl Default for Knobs {
    fn default() -> Self {
        Knobs {
            latency_ms: 0,
            precision_ms: 1,
            insens: false,
            no_ranged_read: false,
            no_set_mtime: false,
            no_fsync: false,
            no_write_at: false,
            no_symlink: false,
            no_unix_mode: false,
            no_free_space: false,
            no_read_back: false,
            no_rename_overwrite: false,
            streams: 4,
            block_size: 64 * 1024,
            transient_after: None,
            fail_read: None,
        }
    }
}

struct Inner {
    phrase: String,
    seed: String,
    knobs: Knobs,
    nodes: Mutex<HashMap<String, FNode>>,
    counters: FakeCounters,
    ops: AtomicU64,
}

pub struct FakeVfs {
    inner: Arc<Inner>,
}

/// Same phrase → same tree (syncthing's fakeFS cache): a test seeds files through one
/// handle and the engine's own `open()` of the same phrase sees them. Distinct trees
/// with *identical generated content* come from the same seed with different knobs
/// (`fake://s` vs `fake://s?streams=2`) — the registry keys on the whole phrase, the
/// generator only on the seed.
static REGISTRY: std::sync::OnceLock<Mutex<HashMap<String, Arc<Inner>>>> = std::sync::OnceLock::new();

impl FakeVfs {
    pub fn from_phrase(raw: &str) -> VfsResult<FakeVfs> {
        let reg = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        {
            let map = reg.lock().unwrap();
            if let Some(inner) = map.get(raw) {
                return Ok(FakeVfs { inner: inner.clone() });
            }
        }
        let v = Self::parse_phrase(raw)?;
        let mut map = reg.lock().unwrap();
        let inner = map.entry(raw.to_string()).or_insert_with(|| v.inner.clone());
        Ok(FakeVfs { inner: inner.clone() })
    }

    fn parse_phrase(raw: &str) -> VfsResult<FakeVfs> {
        let rest = raw
            .strip_prefix("fake://")
            .ok_or_else(|| VfsError::new(VfsErrorKind::Protocol, format!("not a fake:// phrase: {raw}")))?;
        let (seed, query) = match rest.split_once('?') {
            Some((s, q)) => (s, q),
            None => (rest, ""),
        };
        let mut k = Knobs::default();
        for part in query.split('&').filter(|p| !p.is_empty()) {
            let (key, val) = match part.split_once('=') {
                Some((a, b)) => (a, b),
                None => (part, ""),
            };
            match key {
                "latency_ms" => k.latency_ms = val.parse().unwrap_or(0),
                "precision_ms" => k.precision_ms = val.parse().unwrap_or(1).max(1),
                "insens" => k.insens = true,
                "no_ranged_read" => k.no_ranged_read = true,
                "no_set_mtime" => k.no_set_mtime = true,
                "no_fsync" => k.no_fsync = true,
                "no_write_at" => k.no_write_at = true,
                "no_symlink" => k.no_symlink = true,
                "no_unix_mode" => k.no_unix_mode = true,
                "no_free_space" => k.no_free_space = true,
                "no_read_back" => k.no_read_back = true,
                "no_rename_overwrite" => k.no_rename_overwrite = true,
                "streams" => k.streams = val.parse().unwrap_or(4).max(1),
                "block_size" => k.block_size = val.parse().unwrap_or(64 * 1024).max(1),
                "transient_after" => k.transient_after = val.parse().ok(),
                "fail_read" => k.fail_read = Some(val.to_string()),
                other => {
                    return Err(VfsError::new(
                        VfsErrorKind::Protocol,
                        format!("unknown fake:// knob '{other}' — refusing to ignore it silently"),
                    ))
                }
            }
        }
        Ok(FakeVfs {
            inner: Arc::new(Inner {
                phrase: raw.to_string(),
                seed: seed.to_string(),
                knobs: k,
                nodes: Mutex::new(HashMap::new()),
                counters: FakeCounters::default(),
                ops: AtomicU64::new(0),
            }),
        })
    }

    pub fn counters(&self) -> &FakeCounters {
        &self.inner.counters
    }

    // ---- test seeding (bypasses knobs and counters on purpose) ----

    pub fn seed_dir(&self, rel: &str) {
        let mut nodes = self.inner.nodes.lock().unwrap();
        seed_parents(&mut nodes, rel);
        nodes.insert(rel.to_string(), dir_node());
    }

    pub fn seed_file(&self, rel: &str, size: u64, mtime_ms: i64) {
        let mut nodes = self.inner.nodes.lock().unwrap();
        seed_parents(&mut nodes, rel);
        nodes.insert(
            rel.to_string(),
            FNode {
                kind: EntryKind::File,
                size,
                mtime_ms: trunc(mtime_ms, self.inner.knobs.precision_ms),
                mode: Some(0o644),
                link: None,
                content: None,
            },
        );
    }

    pub fn seed_symlink(&self, rel: &str, target: &str, mtime_ms: i64) {
        let mut nodes = self.inner.nodes.lock().unwrap();
        seed_parents(&mut nodes, rel);
        nodes.insert(
            rel.to_string(),
            FNode {
                kind: EntryKind::Symlink,
                size: 0,
                mtime_ms: trunc(mtime_ms, self.inner.knobs.precision_ms),
                mode: None,
                link: Some(target.to_string()),
                content: None,
            },
        );
    }
}

fn dir_node() -> FNode {
    FNode { kind: EntryKind::Dir, size: 0, mtime_ms: 0, mode: Some(0o755), link: None, content: None }
}

fn seed_parents(nodes: &mut HashMap<String, FNode>, rel: &str) {
    let mut prefix = String::new();
    for seg in rel.split('/').collect::<Vec<_>>().split_last().map(|(_, init)| init).unwrap_or(&[]) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(seg);
        nodes.entry(prefix.clone()).or_insert_with(dir_node);
    }
}

fn trunc(ms: i64, precision_ms: u32) -> i64 {
    let p = precision_ms as i64;
    if p <= 1 {
        ms
    } else {
        ms - ms.rem_euclid(p)
    }
}

/// Deterministic bytes: blake3 XOF over (seed, path), positioned at `off`.
fn gen_bytes(seed: &str, rel: &str, off: u64, buf: &mut [u8]) {
    let mut h = blake3::Hasher::new();
    h.update(seed.as_bytes());
    h.update(b"\0");
    h.update(rel.as_bytes());
    let mut reader = h.finalize_xof();
    reader.set_position(off);
    reader.fill(buf);
}

impl Inner {
    /// Every public op passes here: latency injection, op counting, mid-run failure.
    fn gate(&self) -> VfsResult<()> {
        let n = self.ops.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(limit) = self.knobs.transient_after {
            if n > limit {
                return Err(VfsError::new(
                    VfsErrorKind::Transient,
                    format!("injected connection loss (op {n} > {limit})"),
                ));
            }
        }
        if self.knobs.latency_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(self.knobs.latency_ms));
        }
        Ok(())
    }

    /// Resolve a rel against the tree, honoring case-insensitivity.
    fn resolve(&self, nodes: &HashMap<String, FNode>, rel: &str) -> Option<String> {
        if nodes.contains_key(rel) {
            return Some(rel.to_string());
        }
        if self.knobs.insens {
            let want = rel.to_lowercase();
            return nodes.keys().find(|k| k.to_lowercase() == want).cloned();
        }
        None
    }

    fn meta_of(&self, _rel: &str, n: &FNode) -> VMeta {
        VMeta {
            kind: n.kind.clone(),
            size: n.size,
            mtime_ms: n.mtime_ms,
            mode: if self.knobs.no_unix_mode { None } else { n.mode },
            file_id: None,
            link: n.link.clone(),
        }
    }
}

struct FakeRead {
    inner: Arc<Inner>,
    rel: String,
    content: Option<Arc<Vec<u8>>>,
    size: u64,
    pos: u64,
    block: usize,
}

impl std::io::Read for FakeRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.gate().map_err(std::io::Error::from)?;
        if self.pos >= self.size {
            return Ok(0);
        }
        let n = buf.len().min((self.size - self.pos) as usize);
        match &self.content {
            Some(bytes) => {
                let start = self.pos as usize;
                buf[..n].copy_from_slice(&bytes[start..start + n]);
            }
            None => gen_bytes(&self.inner.seed, &self.rel, self.pos, &mut buf[..n]),
        }
        self.pos += n as u64;
        self.inner.counters.bytes_read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}
impl ReadStream for FakeRead {
    fn block_size(&self) -> usize {
        self.block
    }
}

struct FakeStaged {
    inner: Arc<Inner>,
    rel: String,
    hint: WriteHint,
    buf: Vec<u8>,
}

impl WriteStaged for FakeStaged {
    fn write(&mut self, data: &[u8]) -> VfsResult<()> {
        self.inner.gate()?;
        self.buf.extend_from_slice(data);
        self.inner.counters.bytes_written.fetch_add(data.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    fn block_size(&self) -> usize {
        self.inner.knobs.block_size
    }

    fn write_at(&mut self, off: u64, data: &[u8]) -> VfsResult<()> {
        if self.inner.knobs.no_write_at {
            return Err(VfsError::unsupported("write_at disabled on this fake root"));
        }
        self.inner.gate()?;
        let end = off as usize + data.len();
        if self.buf.len() < end {
            self.buf.resize(end, 0);
        }
        self.buf[off as usize..end].copy_from_slice(data);
        self.inner.counters.bytes_written.fetch_add(data.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    fn seal(&mut self, fsync: bool) -> VfsResult<()> {
        if fsync && self.inner.knobs.no_fsync {
            // The last line of defense: preflight should have routed around this.
            return Err(VfsError::unsupported("fsync requested but this root has none"));
        }
        Ok(())
    }

    fn staged_len(&self) -> VfsResult<u64> {
        Ok(self.buf.len() as u64)
    }

    fn open_staged_read(&self) -> VfsResult<Box<dyn ReadStream>> {
        if self.inner.knobs.no_read_back {
            return Err(VfsError::unsupported("read_back disabled on this fake root"));
        }
        Ok(Box::new(FakeRead {
            inner: self.inner.clone(),
            rel: self.rel.clone(),
            content: Some(Arc::new(self.buf.clone())),
            size: self.buf.len() as u64,
            pos: 0,
            block: self.inner.knobs.block_size,
        }))
    }

    fn commit(self: Box<Self>) -> VfsResult<CommitReport> {
        self.inner.gate()?;
        let mut report = CommitReport::default();
        let mtime = match self.hint.mtime_ms {
            Some(ms) if self.inner.knobs.no_set_mtime => {
                report.mtime_error = Some(VfsError::unsupported("set_mtime disabled on this fake root"));
                let _ = ms;
                crate::foundation::time::now_ms() as i64
            }
            Some(ms) => trunc(ms, self.inner.knobs.precision_ms),
            None => crate::foundation::time::now_ms() as i64,
        };
        let mut nodes = self.inner.nodes.lock().unwrap();
        seed_parents(&mut nodes, &self.rel);
        nodes.insert(
            self.rel.clone(),
            FNode {
                kind: EntryKind::File,
                size: self.buf.len() as u64,
                mtime_ms: mtime,
                mode: self.hint.mode.or(Some(0o644)),
                link: None,
                content: Some(Arc::new(self.buf)),
            },
        );
        report.mtime_ondisk_ms = Some(mtime);
        Ok(report)
    }
}

impl Vfs for FakeVfs {
    fn caps(&self) -> VfsCaps {
        let k = &self.inner.knobs;
        let no = |b: bool| if b { Support::No } else { Support::Yes };
        VfsCaps {
            protocol: "fake",
            mtime_precision_ms: k.precision_ms,
            set_mtime: no(k.no_set_mtime),
            fsync: no(k.no_fsync),
            rename: Support::Yes,
            rename_overwrite: no(k.no_rename_overwrite),
            ranged_read: no(k.no_ranged_read),
            write_at: no(k.no_write_at),
            unix_mode: no(k.no_unix_mode),
            symlink: no(k.no_symlink),
            file_id: Support::No,
            free_space: no(k.no_free_space),
            read_back: no(k.no_read_back),
            local_trash: false,
            case_sensitivity: if k.insens { CaseSense::Insensitive } else { CaseSense::Sensitive },
            max_parallel_streams: k.streams,
        }
    }

    fn display(&self) -> String {
        self.inner.phrase.clone()
    }

    fn identity(&self) -> String {
        self.inner.phrase.clone()
    }

    fn connect(&self) -> VfsResult<()> {
        self.inner.gate()
    }

    fn stat(&self, rel: &str) -> VfsResult<Option<VMeta>> {
        self.inner.gate()?;
        self.inner.counters.stat.fetch_add(1, Ordering::Relaxed);
        if rel.is_empty() {
            return Ok(Some(VMeta {
                kind: EntryKind::Dir,
                size: 0,
                mtime_ms: 0,
                mode: Some(0o755),
                file_id: None,
                link: None,
            }));
        }
        let nodes = self.inner.nodes.lock().unwrap();
        Ok(self
            .inner
            .resolve(&nodes, rel)
            .map(|k| self.inner.meta_of(&k, &nodes[&k])))
    }

    fn read_dir(&self, rel: &str) -> VfsResult<Vec<VDirEntry>> {
        self.inner.gate()?;
        self.inner.counters.read_dir.fetch_add(1, Ordering::Relaxed);
        let nodes = self.inner.nodes.lock().unwrap();
        let dir = if rel.is_empty() {
            String::new()
        } else {
            match self.inner.resolve(&nodes, rel) {
                Some(k) if nodes[&k].kind == EntryKind::Dir => k,
                Some(k) => {
                    return Err(VfsError::new(
                        VfsErrorKind::Io,
                        format!("not a directory: {k}"),
                    ))
                }
                None => return Err(VfsError::new(VfsErrorKind::NotFound, format!("no such directory: {rel}"))),
            }
        };
        let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
        let mut out = Vec::new();
        for (k, n) in nodes.iter() {
            if let Some(rest) = k.strip_prefix(&prefix) {
                if !rest.is_empty() && !rest.contains('/') {
                    out.push(VDirEntry { name: rest.to_string(), meta: self.inner.meta_of(k, n) });
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>> {
        self.inner.gate()?;
        self.inner.counters.open_read.fetch_add(1, Ordering::Relaxed);
        if let Some(pat) = &self.inner.knobs.fail_read {
            if rel.contains(pat.as_str()) {
                return Err(VfsError::new(VfsErrorKind::Io, format!("injected read failure on {rel}")));
            }
        }
        let nodes = self.inner.nodes.lock().unwrap();
        let key = self
            .inner
            .resolve(&nodes, rel)
            .ok_or_else(|| VfsError::new(VfsErrorKind::NotFound, format!("no such file: {rel}")))?;
        let n = &nodes[&key];
        Ok(Box::new(FakeRead {
            inner: self.inner.clone(),
            rel: key.clone(),
            content: n.content.clone(),
            size: n.size,
            pos: 0,
            block: self.inner.knobs.block_size,
        }))
    }

    fn read_range(&self, rel: &str, off: u64, len: u32) -> VfsResult<Vec<u8>> {
        if self.inner.knobs.no_ranged_read {
            return Err(VfsError::unsupported("ranged reads disabled on this fake root"));
        }
        self.inner.gate()?;
        self.inner.counters.read_range.fetch_add(1, Ordering::Relaxed);
        if let Some(pat) = &self.inner.knobs.fail_read {
            if rel.contains(pat.as_str()) {
                return Err(VfsError::new(VfsErrorKind::Io, format!("injected read failure on {rel}")));
            }
        }
        let nodes = self.inner.nodes.lock().unwrap();
        let key = self
            .inner
            .resolve(&nodes, rel)
            .ok_or_else(|| VfsError::new(VfsErrorKind::NotFound, format!("no such file: {rel}")))?;
        let n = &nodes[&key];
        if off >= n.size {
            return Ok(Vec::new());
        }
        let take = (len as u64).min(n.size - off) as usize;
        let mut buf = vec![0u8; take];
        match &n.content {
            Some(bytes) => buf.copy_from_slice(&bytes[off as usize..off as usize + take]),
            None => gen_bytes(&self.inner.seed, &key, off, &mut buf),
        }
        self.inner.counters.bytes_read.fetch_add(take as u64, Ordering::Relaxed);
        Ok(buf)
    }

    fn read_link(&self, rel: &str) -> VfsResult<String> {
        self.inner.gate()?;
        let nodes = self.inner.nodes.lock().unwrap();
        let key = self
            .inner
            .resolve(&nodes, rel)
            .ok_or_else(|| VfsError::new(VfsErrorKind::NotFound, format!("no such link: {rel}")))?;
        nodes[&key]
            .link
            .clone()
            .ok_or_else(|| VfsError::new(VfsErrorKind::Io, format!("not a symlink: {rel}")))
    }

    fn mkdir_all(&self, rel: &str) -> VfsResult<()> {
        self.inner.gate()?;
        let mut nodes = self.inner.nodes.lock().unwrap();
        seed_parents(&mut nodes, rel);
        if !rel.is_empty() {
            nodes.entry(rel.to_string()).or_insert_with(dir_node);
        }
        Ok(())
    }

    fn open_write(&self, rel: &str, hint: &WriteHint) -> VfsResult<Box<dyn WriteStaged>> {
        self.inner.gate()?;
        self.inner.counters.open_write.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(FakeStaged {
            inner: self.inner.clone(),
            rel: rel.to_string(),
            hint: hint.clone(),
            buf: Vec::new(),
        }))
    }

    fn rename(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        self.inner.gate()?;
        let mut nodes = self.inner.nodes.lock().unwrap();
        let from = self
            .inner
            .resolve(&nodes, from_rel)
            .ok_or_else(|| VfsError::new(VfsErrorKind::NotFound, format!("no such path: {from_rel}")))?;
        if self.inner.resolve(&nodes, to_rel).is_some() && self.inner.knobs.no_rename_overwrite {
            return Err(VfsError::new(
                VfsErrorKind::AlreadyExists,
                format!("destination exists and this root refuses rename-overwrite: {to_rel}"),
            ));
        }
        // Move the node and, for directories, the whole subtree under it
        let node = nodes.remove(&from).unwrap();
        let is_dir = node.kind == EntryKind::Dir;
        seed_parents(&mut nodes, to_rel);
        nodes.insert(to_rel.to_string(), node);
        if is_dir {
            let prefix = format!("{from}/");
            let moved: Vec<String> = nodes.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
            for k in moved {
                let v = nodes.remove(&k).unwrap();
                let new_key = format!("{to_rel}/{}", &k[prefix.len()..]);
                nodes.insert(new_key, v);
            }
        }
        Ok(())
    }

    fn remove_file(&self, rel: &str) -> VfsResult<()> {
        self.inner.gate()?;
        let mut nodes = self.inner.nodes.lock().unwrap();
        let key = self
            .inner
            .resolve(&nodes, rel)
            .ok_or_else(|| VfsError::new(VfsErrorKind::NotFound, format!("no such file: {rel}")))?;
        nodes.remove(&key);
        Ok(())
    }

    fn remove_dir(&self, rel: &str) -> VfsResult<()> {
        self.inner.gate()?;
        let mut nodes = self.inner.nodes.lock().unwrap();
        let key = self
            .inner
            .resolve(&nodes, rel)
            .ok_or_else(|| VfsError::new(VfsErrorKind::NotFound, format!("no such directory: {rel}")))?;
        let prefix = format!("{key}/");
        if nodes.keys().any(|k| k.starts_with(&prefix)) {
            return Err(VfsError::new(VfsErrorKind::NotEmpty, format!("directory not empty: {key}")));
        }
        nodes.remove(&key);
        Ok(())
    }

    fn set_mtime(&self, rel: &str, mtime_ms: i64) -> VfsResult<()> {
        if self.inner.knobs.no_set_mtime {
            return Err(VfsError::unsupported("set_mtime disabled on this fake root"));
        }
        self.inner.gate()?;
        let mut nodes = self.inner.nodes.lock().unwrap();
        let key = self
            .inner
            .resolve(&nodes, rel)
            .ok_or_else(|| VfsError::new(VfsErrorKind::NotFound, format!("no such path: {rel}")))?;
        let p = self.inner.knobs.precision_ms;
        nodes.get_mut(&key).unwrap().mtime_ms = trunc(mtime_ms, p);
        Ok(())
    }

    fn set_mode(&self, rel: &str, mode: u32) -> VfsResult<()> {
        if self.inner.knobs.no_unix_mode {
            return Err(VfsError::unsupported("unix modes disabled on this fake root"));
        }
        self.inner.gate()?;
        let mut nodes = self.inner.nodes.lock().unwrap();
        let key = self
            .inner
            .resolve(&nodes, rel)
            .ok_or_else(|| VfsError::new(VfsErrorKind::NotFound, format!("no such path: {rel}")))?;
        nodes.get_mut(&key).unwrap().mode = Some(mode);
        Ok(())
    }

    fn make_symlink(&self, rel: &str, target: &str) -> VfsResult<()> {
        if self.inner.knobs.no_symlink {
            return Err(VfsError::unsupported("symlinks disabled on this fake root"));
        }
        self.inner.gate()?;
        let mut nodes = self.inner.nodes.lock().unwrap();
        seed_parents(&mut nodes, rel);
        nodes.insert(
            rel.to_string(),
            FNode {
                kind: EntryKind::Symlink,
                size: 0,
                mtime_ms: crate::foundation::time::now_ms() as i64,
                mode: None,
                link: Some(target.to_string()),
                content: None,
            },
        );
        Ok(())
    }

    fn free_space(&self) -> VfsResult<Option<(u64, u64)>> {
        self.inner.gate()?;
        if self.inner.knobs.no_free_space {
            return Ok(None);
        }
        Ok(Some((10 << 30, 20 << 30)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn mk(phrase: &str) -> FakeVfs {
        FakeVfs::from_phrase(phrase).unwrap()
    }

    #[test]
    fn same_seed_agrees_on_every_byte() {
        // Same seed, different knob tail = two *independent* trees whose generated
        // bytes agree — the registry keys on the whole phrase, the generator on the seed
        let a = mk("fake://seedtest");
        let b = mk("fake://seedtest?block_size=131072");
        a.seed_file("dir/f.bin", 100_000, 1_000);
        b.seed_file("dir/f.bin", 100_000, 1_000);
        let mut ba = Vec::new();
        let mut bb = Vec::new();
        a.open_read("dir/f.bin").unwrap().read_to_end(&mut ba).unwrap();
        b.open_read("dir/f.bin").unwrap().read_to_end(&mut bb).unwrap();
        assert_eq!(ba, bb);
        assert_eq!(ba.len(), 100_000);
        // and a different seed disagrees
        let c = mk("fake://seedtest2");
        c.seed_file("dir/f.bin", 100_000, 1_000);
        let mut bc = Vec::new();
        c.open_read("dir/f.bin").unwrap().read_to_end(&mut bc).unwrap();
        assert_ne!(ba, bc);
    }

    #[test]
    fn same_phrase_shares_one_tree() {
        let a = mk("fake://sharedtree");
        a.seed_file("x.bin", 5, 0);
        let b = mk("fake://sharedtree");
        assert!(b.stat("x.bin").unwrap().is_some(), "same phrase must open the same tree");
    }

    #[test]
    fn ranged_read_matches_stream() {
        let v = mk("fake://rrtest");
        v.seed_file("f.bin", 10_000, 0);
        let mut full = Vec::new();
        v.open_read("f.bin").unwrap().read_to_end(&mut full).unwrap();
        let mid = v.read_range("f.bin", 4_000, 256).unwrap();
        assert_eq!(&full[4_000..4_256], &mid[..]);
        // EOF-truncated window
        let tail = v.read_range("f.bin", 9_900, 256).unwrap();
        assert_eq!(tail.len(), 100);
        assert_eq!(&full[9_900..], &tail[..]);
    }

    #[test]
    fn precision_truncates_mtimes() {
        let v = mk("fake://s?precision_ms=60000");
        v.seed_file("f", 1, 1_234_567);
        assert_eq!(v.stat("f").unwrap().unwrap().mtime_ms, 1_200_000);
    }

    #[test]
    fn transient_after_cuts_the_line() {
        let v = mk("fake://s?transient_after=3");
        v.seed_file("f", 1, 0);
        assert!(v.stat("f").is_ok()); // 1
        assert!(v.stat("f").is_ok()); // 2
        assert!(v.stat("f").is_ok()); // 3
        let e = v.stat("f").unwrap_err();
        assert_eq!(e.kind, VfsErrorKind::Transient);
    }

    #[test]
    fn insens_resolves_but_preserves_real_names() {
        let v = mk("fake://s?insens");
        v.seed_file("Dir/File.TXT", 5, 0);
        assert!(v.stat("dir/file.txt").unwrap().is_some());
        let names: Vec<String> = v.read_dir("Dir").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["File.TXT"]);
    }

    #[test]
    fn written_content_wins_over_generated() {
        let v = mk("fake://writetest");
        let mut w = v.open_write("out/x.bin", &WriteHint { mtime_ms: Some(9_000), ..Default::default() }).unwrap();
        w.write(b"hello world").unwrap();
        assert_eq!(w.staged_len().unwrap(), 11);
        // invisible before commit
        assert!(v.stat("out/x.bin").unwrap().is_none());
        let report = w.commit().unwrap();
        assert_eq!(report.mtime_ondisk_ms, Some(9_000));
        let mut got = Vec::new();
        v.open_read("out/x.bin").unwrap().read_to_end(&mut got).unwrap();
        assert_eq!(got, b"hello world");
    }

    #[test]
    fn counters_count() {
        let v = mk("fake://countertest");
        v.seed_file("a/f1", 10, 0);
        v.seed_file("a/f2", 10, 0);
        let _ = v.read_dir("a").unwrap();
        let _ = v.open_read("a/f1").unwrap();
        assert_eq!(v.counters().read_dir.load(Ordering::Relaxed), 1);
        assert_eq!(v.counters().open_read.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn unknown_knob_is_refused() {
        assert!(FakeVfs::from_phrase("fake://s?latencyms=5").is_err());
    }
}
