//! The virtual filesystem a sync root lives on.
//!
//! One `Vfs` instance = one root. Every path is a table rel: '/'-separated, no
//! leading or trailing '/', `""` names the root itself, `foundation::path::is_safe_rel`
//! holds. The engine walks, prunes, counts exclusions and drives the copy loops;
//! a backend only answers per-directory and per-file primitives.
//!
//! Layering note: this sits in L0 `fs` and reaches sideways into L0 `model` for
//! `EntryKind` — the table vocabulary is the lingua franca between scanner and
//! backend, and `model` uses nothing from `fs`, so the graph stays acyclic.
//!
//! Design lineage (from the FreeFileSync AFS layer and syncthing's lib/fs, both
//! surveyed in-repo under `.libs/`):
//! - capabilities are declared (`caps()`), not discovered by runtime failure —
//!   preflight routes around missing ones *before* work starts, and every
//!   degradation is surfaced, never silent;
//! - `stat` returning `Ok(None)` is a **confirmed** absence; ambiguous protocol
//!   errors must resolve to `Transient` instead (see `error.rs` for why);
//! - writes stage into a temp name and land by rename (`WriteStaged`), so an
//!   interruption anywhere leaves either the old file or the new one, never half;
//! - mtime that fails to stick is a report, not a failure (`CommitReport`) — FFS's
//!   errorModTime lesson: plenty of real servers can copy bytes but not set times.

pub mod error;
pub mod spec;

pub mod cred;
pub mod ftp;
pub mod local;
pub mod sftp;
pub mod smb;

#[cfg(test)]
pub mod conformance;
#[cfg(test)]
pub mod memory;

use std::path::Path;
use std::sync::Arc;

use crate::model::table::EntryKind;
use error::{VfsError, VfsErrorKind, VfsResult};
use spec::{RemoteSpec, RootSpec};

/// A live probe result. `model::table::Entry` is the *table* format; `VMeta` is what
/// the filesystem said just now.
#[derive(Clone, Debug)]
pub struct VMeta {
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_ms: i64,
    /// Unix permission bits. None = the backend genuinely has none (says so, rather
    /// than inventing 0o644).
    pub mode: Option<u32>,
    /// Stable file identity (`dev:inode` on unix). Only ever corroborates move
    /// detection; None is normal.
    pub file_id: Option<String>,
    /// Symlink target. None does **not** mean "no target" — it means "fetching it
    /// costs an extra round-trip, call read_link if you care" (FFS's
    /// tryGetAttributesFast Option-semantics).
    pub link: Option<String>,
}

#[derive(Clone, Debug)]
pub struct VDirEntry {
    /// Single path segment, never containing '/'.
    pub name: String,
    pub meta: VMeta,
}

/// Three-valued capability: `Unknown` is honest for "can't tell before connect()"
/// and preflight lists it as such instead of guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Yes,
    No,
    Unknown,
}

impl Support {
    pub fn yes(self) -> bool {
        self == Support::Yes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseSense {
    Sensitive,
    Insensitive,
    /// Declared by configuration, not probed at run time (syncthing's stance).
    Unknown,
}

/// What kind of storage a root sits on.
///
/// A local *path* says nothing about this: `D:\photos` and `\\nas\photos` are both
/// `RootSpec::Local` and both take the walkdir fast path, yet only one of them can have its
/// deletions renamed into a trash store on this machine. Local roots probe it on first use
/// (`local::LocalVfs::volume`); protocol backends know it by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Medium {
    FixedDisk,
    RemovableDisk,
    /// Reached over a network — a mounted share, or a protocol backend's own link.
    NetworkShare,
    /// The probe could not tell. Treated as "assume nothing", never as "assume local".
    Unknown,
}

impl Medium {
    pub fn as_str(self) -> &'static str {
        match self {
            Medium::FixedDisk => "fixed disk",
            Medium::RemovableDisk => "removable disk",
            Medium::NetworkShare => "network share",
            Medium::Unknown => "unknown",
        }
    }
}

/// Which naming rules a write to this root is subject to.
///
/// **This is a property of the path layer the write travels through, not of the far machine.**
/// A Windows client writing to a Linux Samba box over a mounted `\\host\share` still gets Win32
/// parsing — `a:b` turns into an alternate-data-stream write on the client side before a single
/// packet leaves — and such a root is a plain `RootSpec::Local`, so it reports the host's rules.
/// The protocol backends bypass that layer entirely and put the name on the wire as spelled, so
/// what governs is the *server's* rules and its OS is not visible from here: smb, sftp and ftp
/// all report `Unknown`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameRules {
    /// Reserved device names; no `< > : " | ? *`; no trailing dot or space.
    Windows,
    /// Anything except `/` and NUL.
    Posix,
    /// Not knowable from here. The engine warns instead of refusing, and says which.
    Unknown,
}

impl NameRules {
    pub fn as_str(self) -> &'static str {
        match self {
            NameRules::Windows => "windows",
            NameRules::Posix => "posix",
            NameRules::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> NameRules {
        match s {
            "windows" => NameRules::Windows,
            "posix" => NameRules::Posix,
            _ => NameRules::Unknown,
        }
    }

    /// What a root reached through this process's own path layer is subject to.
    pub fn host() -> NameRules {
        if cfg!(windows) { NameRules::Windows } else { NameRules::Posix }
    }
}

/// What a backend can do. Authoritative only after `connect()` — FTP learns MFMT/MLSD
/// from FEAT, SFTP learns fsync@openssh.com from the extension list.
#[derive(Clone, Debug)]
pub struct VfsCaps {
    pub protocol: &'static str,
    /// Storage granularity of mtimes, in ms. Feeds the compare window:
    /// local NTFS/APFS ≈ 1, FAT = 2000, SFTP = 1000, FTP+MLSD = 1000, FTP LIST-only = 60000.
    pub mtime_precision_ms: u32,
    pub set_mtime: Support,
    pub fsync: Support,
    pub rename: Support,
    /// No = the engine clears the destination first (it does anyway; this simply
    /// records whose job the overwrite is).
    pub rename_overwrite: Support,
    /// Random-access reads (sampled evidence = 3 windows). No → preflight degrades
    /// the evidence tier, out loud.
    pub ranged_read: Support,
    /// Random-access writes into the staged file (delta updates).
    pub write_at: Support,
    pub unix_mode: Support,
    pub symlink: Support,
    pub file_id: Support,
    pub free_space: Support,
    /// Re-reading the staged file before commit (verify_writes' strong form).
    pub read_back: Support,
    /// What kind of storage this root sits on. Local roots measure it; protocol backends
    /// report `NetworkShare` by construction.
    pub medium: Medium,
    /// The central trash store may take this root's deletions.
    ///
    /// False does **not** mean "cannot preserve" — it means "preserving here would copy the
    /// bytes off this root", so the originals are renamed into `<root>/.syncdash/trash/<run>/`
    /// instead. That is the whole point: for a network share, a move into the local trash is a
    /// download of every deleted file. A cross-volume move between two disks on this machine is
    /// merely a local copy, so removable and fixed volumes both keep the central store.
    pub local_trash: bool,
    pub case_sensitivity: CaseSense,
    /// Naming rules writes to this root must satisfy. Drives the plan-time legality
    /// preflight; `Unknown` downgrades that from a refusal to a visible warning.
    pub name_rules: NameRules,
    /// Ceiling for concurrent streams the backend is comfortable with (pool width,
    /// scan hash width, apply copy width all clamp to it).
    pub max_parallel_streams: usize,
}

/// Advance notice for a staged write. The backend chooses when to apply what
/// (local sets mtime on the temp file; SFTP sets it after close by path; FTP can
/// only MFMT after rename) — the engine only states intent.
#[derive(Clone, Debug, Default)]
pub struct WriteHint {
    pub size_hint: Option<u64>,
    pub mtime_ms: Option<i64>,
    pub mode: Option<u32>,
}

/// What actually happened at commit. mtime/mode failures ride here instead of
/// failing the copy — the engine decides (records a correction, or surfaces per
/// the preflight agreement). Silence is not an option; error-return is not either.
#[derive(Debug, Default)]
pub struct CommitReport {
    /// The mtime observed on the final file right after commit, when the backend
    /// can see it cheaply. Feeds the mtime-correction table.
    pub mtime_ondisk_ms: Option<i64>,
    pub mtime_error: Option<VfsError>,
    pub mode_error: Option<VfsError>,
}

/// Read stream: plain `std::io::Read` (feeds blake3 / io::copy unchanged) plus the
/// backend's preferred chunk size — FFS measured these per protocol (SFTP ≈ 480 KiB,
/// FTP ≈ 64 KiB, local 1 MiB) and the engine sizes its buffers accordingly.
pub trait ReadStream: std::io::Read + Send {
    fn block_size(&self) -> usize;
}

/// A staged write: the abstract twin of `fs::staged::Staged`. Data goes to a temp
/// name (based on `foundation::names::TEMP_PREFIX` — never a `~` prefix, some FTP
/// servers silently rewrite those) and only `commit` makes it visible.
///
/// Drop contract: a `WriteStaged` dropped without commit removes its temp file.
/// An interruption at any point leaves no debris and an untouched destination.
pub trait WriteStaged: Send {
    fn write(&mut self, buf: &[u8]) -> VfsResult<()>;
    fn block_size(&self) -> usize;
    /// Random-access patch (delta path). Only when `caps().write_at` says Yes.
    fn write_at(&mut self, off: u64, buf: &[u8]) -> VfsResult<()>;
    /// Flush (+fsync when asked and able) and close the data handle. Must precede
    /// commit — Windows renames fail with an open write handle.
    fn seal(&mut self, fsync: bool) -> VfsResult<()>;
    /// Bytes the backend actually holds for the staged file. The engine reconciles
    /// this against its own count (FFS's finalize check — it has caught corrupt
    /// SFTP downloads in the wild).
    fn staged_len(&self) -> VfsResult<u64>;
    /// Re-read the staged content (verify_writes). Only when `caps().read_back` says Yes.
    fn open_staged_read(&self) -> VfsResult<Box<dyn ReadStream>>;
    /// Escape hatch: when the staged file is a real local path, verification uses
    /// mmap+rayon on it instead of a streaming re-read.
    fn local_path(&self) -> Option<&Path> {
        None
    }
    /// Atomically move the staged file onto the destination. The engine has already
    /// cleared/preserved the old file per plan. Consumes self.
    fn commit(self: Box<Self>) -> VfsResult<CommitReport>;
}

/// One sync root on some filesystem. All methods take `&self` and must be callable
/// from multiple threads; connection pooling is the backend's own affair.
pub trait Vfs: Send + Sync {
    // -------- identity & capability --------
    fn caps(&self) -> VfsCaps;
    /// Credential-free display string: table headers, plan headers, logs, UI.
    fn display(&self) -> String;
    /// Credential-free canonical identity: hash-cache key, mtime-correction key,
    /// lock ownership. For a local root this is the root string exactly as spelled
    /// (existing caches must keep hitting).
    fn identity(&self) -> String;
    /// Local escape hatch: a real directory the engine may touch with std::fs,
    /// walkdir and mmap. `Some` routes the whole fast path.
    fn as_local(&self) -> Option<&Path> {
        None
    }
    /// One line of server truth for the log ("OpenSSH_9.6, fsync@openssh.com: yes").
    fn server_info(&self) -> Option<String> {
        None
    }

    // -------- session --------
    /// Establish and authenticate. Idempotent; cheap when already connected.
    /// Missing credentials are a hard `Auth` error — never an anonymous fallback.
    fn connect(&self) -> VfsResult<()>;

    // -------- read side --------
    /// lstat semantics (never follows links). `Ok(None)` = confirmed absent, see
    /// module docs. `rel = ""` probes the root itself.
    fn stat(&self, rel: &str) -> VfsResult<Option<VMeta>>;
    /// One directory level, names + metadata in one round-trip.
    fn read_dir(&self, rel: &str) -> VfsResult<Vec<VDirEntry>>;
    /// Names-only listing for callers that need nothing else (delete-dir residue
    /// sampling). Default delegates; backends with a cheaper primitive override.
    fn read_dir_names(&self, rel: &str) -> VfsResult<Vec<(String, EntryKind)>> {
        Ok(self.read_dir(rel)?.into_iter().map(|e| (e.name, e.meta.kind)).collect())
    }
    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>>;
    /// Read `len` bytes at `off` (sampled evidence windows). Short only at EOF.
    /// Only when `caps().ranged_read` says Yes.
    fn read_range(&self, rel: &str, off: u64, len: u32) -> VfsResult<Vec<u8>>;
    fn read_link(&self, rel: &str) -> VfsResult<String>;

    // -------- write side --------
    fn mkdir_all(&self, rel: &str) -> VfsResult<()>;
    fn open_write(&self, rel: &str, hint: &WriteHint) -> VfsResult<Box<dyn WriteStaged>>;
    /// Pure rename inside the root. Overwrite behavior is whatever
    /// `caps().rename_overwrite` declares; the engine clears the target first when
    /// it needs the overwrite semantics.
    fn rename(&self, from_rel: &str, to_rel: &str) -> VfsResult<()>;
    fn remove_file(&self, rel: &str) -> VfsResult<()>;
    /// Empty directories only; `NotEmpty` otherwise (the engine classifies).
    fn remove_dir(&self, rel: &str) -> VfsResult<()>;
    fn set_mtime(&self, rel: &str, mtime_ms: i64) -> VfsResult<()>;
    fn set_mode(&self, rel: &str, mode: u32) -> VfsResult<()>;
    fn make_symlink(&self, rel: &str, target: &str) -> VfsResult<()>;

    // -------- root level --------
    /// (free, total) in bytes. `Ok(None)` = honestly unavailable (preflight says so
    /// instead of skipping the space gate silently).
    fn free_space(&self) -> VfsResult<Option<(u64, u64)>>;
}

/// What a backend needs to authenticate. Secrets stay in memory; nothing here is
/// ever serialized.
#[derive(Clone, Default)]
pub struct Credentials {
    pub password: Option<String>,
    pub keyfile: Option<std::path::PathBuf>,
    pub passphrase: Option<String>,
    pub use_agent: bool,
    /// Explicit anonymous (phrase said `anonymous@`). Never a fallback value.
    pub anonymous: bool,
}

/// Where credentials come from. Three impls by shell: CLI = tty prompt, desktop =
/// dialog round-trip, headless = `NoPrompt` (hard error with the remedy spelled out).
pub trait CredentialProvider: Send + Sync {
    fn credentials_for(&self, spec: &RemoteSpec) -> VfsResult<Credentials>;
}

/// Route a root phrase to a live backend. Every scheme here is spoken in this process; an
/// unknown one is refused rather than quietly read as a local path.
pub fn open(phrase: &str, creds: &Arc<dyn CredentialProvider>) -> VfsResult<Arc<dyn Vfs>> {
    match spec::parse(phrase) {
        RootSpec::Local(p) => Ok(Arc::new(local::LocalVfs::new(p))),
        // Needs a stored credential where `\\host\share` needs none — `smb`'s module doc has
        // the whole trade.
        RootSpec::Remote(r) if r.scheme == "smb" => {
            Ok(Arc::new(smb::SmbBackend::new(r, creds.clone())?))
        }
        RootSpec::Remote(r) if r.scheme == "sftp" => {
            Ok(Arc::new(sftp::SftpBackend::new(r, creds.clone())))
        }
        RootSpec::Remote(r) if r.scheme == "ftp" || r.scheme == "ftps" => {
            Ok(Arc::new(ftp::FtpBackend::new(r, creds.clone())))
        }
        // A peer root is not something this process opens: the far side's own syncdash reads and
        // writes it, and `run::peer` drives that over ssh. Reaching here means a caller took a
        // peer job down the in-process lane, which would sync against the wrong filesystem.
        RootSpec::Remote(r) if spec::is_peer_scheme(&r.scheme) => Err(VfsError::new(
            VfsErrorKind::Unsupported,
            format!(
                "{} names a peer root — the syncdash on the far side owns it, so there is no backend to open here (run::peer drives it)",
                r.display()
            ),
        )),
        RootSpec::Remote(r) => Err(VfsError::new(
            VfsErrorKind::Unsupported,
            format!("{}:// has no backend (root: {})", r.scheme, r.display()),
        )),
        RootSpec::UnknownScheme { scheme, raw } => Err(VfsError::new(
            VfsErrorKind::Unsupported,
            format!(
                "unknown scheme '{scheme}://' in '{raw}' — refusing to treat it as a local path; known schemes: {}",
                spec::KNOWN_SCHEMES.join(", ")
            ),
        )),
    }
}
