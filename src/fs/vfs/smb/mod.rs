//! The SMB backend: an in-process SMB2 client, no OS involvement.
//!
//! `smb://` used to be a *translation* route — it handed the phrase to the operating system's own
//! client (a UNC path on Windows, `mount_smbfs` on macOS, nothing at all on Linux) and delegated
//! every call to `LocalVfs` on the resulting path. That worked, and it was the baseline this had
//! to match, but it cost a mount, three platform implementations, and a whole class of bugs that
//! only appear on the OS nobody is building on — one of which sat in the tree through an entire
//! restructure because Windows never compiled the macOS branch. It is gone; this speaks SMB2.
//!
//! What it buys: Linux, no mount orchestration, and one code path instead of three.
//! What it costs: **credentials became mandatory**. The crate is `#![forbid(unsafe_code)]`, so
//! there is no SSPI and no way to ride the Windows session login. That trade was made knowingly
//! and the zero-configuration route did not disappear with it — `\\host\share` still parses as
//! `RootSpec::Local`, still uses the login you already have, and still needs no code at all. It
//! simply stopped being the thing `smb://` secretly meant.
//!
//! Concurrency: one SMB session. `smb2::Connection` is a cheap `Arc` clone whose clones
//! multiplex over the same session, so operations run concurrently without a lock and the
//! `Vfs` trait's `&self` contract is met honestly. The tokio runtime is private to the
//! backend, entered only through `block_on`; the engine never sees an async type.
//!
//! Admission was earned, not assumed: `smb_backend_conforms` runs the same twelve-check contract
//! against a live server that the OS route was measured on, `set_mtime` included — the one method
//! the crate has no high-level call for, and the one `fs::lock`'s heartbeat depends on.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::error::{VfsError, VfsErrorKind, VfsResult};
use super::spec::RemoteSpec;
use super::{
    CaseSense, CredentialProvider, Medium, NameRules, ReadStream, Support, VDirEntry, VMeta, Vfs,
    VfsCaps, WriteHint, WriteStaged,
};
use crate::model::table::EntryKind;

use smb2::client::connection::{CompoundOp, Connection};
use smb2::msg::close::CloseRequest;
use smb2::msg::create::{
    CreateDisposition, CreateRequest, CreateResponse, ImpersonationLevel, ShareAccess,
};
use smb2::msg::set_info::{InfoType, SetInfoRequest};
use smb2::pack::{ReadCursor, Unpack};
use smb2::types::flags::FileAccessMask;
use smb2::types::status::NtStatus;
use smb2::types::{Command, FileId, OplockLevel};
use smb2::{ClientConfig, SmbClient, Tree};

mod basic_info;
mod staged;

use staged::{SmbRead, SmbStaged};

/// `STATUS_DIRECTORY_NOT_EMPTY`. The crate's NtStatus table has no constant for it and its
/// `ErrorKind` folds it into `Other`, but the engine's whole delete-dir classification rides
/// on telling this one apart from a generic protocol error, so it is matched on the raw code.
const STATUS_DIRECTORY_NOT_EMPTY: u32 = 0xC000_0101;

pub struct SmbBackend {
    spec: RemoteSpec,
    creds: Arc<dyn CredentialProvider>,
    /// Share name (first segment of the phrase root); the tree is connected to this.
    share: String,
    /// Path below the share named by the phrase. Every `rel` is resolved under it.
    sub: String,
    timeout: Duration,
    /// Declared ahead of `rt` deliberately: struct fields drop in declaration order, and the
    /// session's teardown aborts a task belonging to that runtime. Dropping the runtime first
    /// would leave it reaching into an executor that no longer exists.
    conn: OnceLock<Conn>,
    rt: tokio::runtime::Runtime,
    connect_gate: Mutex<()>,
    server_line: Mutex<Option<String>>,
}

struct Conn {
    /// Held only to keep the session and its receiver task alive; every operation runs on a
    /// clone of `conn` instead, because the client's own methods want `&mut self` and would
    /// serialize the backend behind one lock.
    _client: SmbClient,
    conn: Connection,
    tree: Arc<Tree>,
}

impl SmbBackend {
    pub fn new(spec: RemoteSpec, creds: Arc<dyn CredentialProvider>) -> VfsResult<SmbBackend> {
        let mut segs = spec.root.splitn(2, '/');
        let share = segs.next().unwrap_or("").to_string();
        if share.is_empty() {
            return Err(VfsError::new(
                VfsErrorKind::Protocol,
                format!(
                    "'{}' names no share — an smb root needs at least smb://host/share",
                    spec.display()
                ),
            ));
        }
        let sub = segs.next().unwrap_or("").trim_matches('/').to_string();
        let timeout = spec
            .opt("timeout")
            .and_then(|t| t.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(20));
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_io()
            .enable_time()
            .thread_name("syncdash-smb")
            .build()
            .expect("tokio runtime");
        Ok(SmbBackend {
            spec,
            creds,
            share,
            sub,
            timeout,
            conn: OnceLock::new(),
            rt,
            connect_gate: Mutex::new(()),
            server_line: Mutex::new(None),
        })
    }

    fn conn(&self) -> VfsResult<&Conn> {
        self.conn.get().ok_or_else(|| {
            VfsError::new(
                VfsErrorKind::Transient,
                format!("'{}' is not connected — connect() must run first", self.spec.display()),
            )
        })
    }

    /// A table rel resolved against the phrase's own subdirectory, still '/'-separated.
    ///
    /// This is what the crate's own methods want: they run `format_path` over whatever they
    /// are given. Only the hand-rolled compound in `set_mtime` needs `wire_path` instead.
    fn share_rel(&self, rel: &str) -> String {
        match (self.sub.as_str(), rel) {
            ("", r) => r.to_string(),
            (s, "") => s.to_string(),
            (s, r) => format!("{s}/{r}"),
        }
    }

    /// `share_rel`, then the normalization the crate applies on the way to the wire.
    ///
    /// `Tree::format_path` is `pub(crate)`, so its rule is restated rather than called:
    /// '/' becomes '\', no leading separator, and a DFS tree wants `server\share\` in front.
    fn wire_path(&self, rel: &str) -> String {
        let p = self.share_rel(rel);
        let normalized = p.replace('/', "\\");
        let normalized = normalized.trim_start_matches('\\');
        let tree = match self.conn.get() {
            Some(c) => &c.tree,
            None => return normalized.to_string(),
        };
        if !tree.is_dfs {
            return normalized.to_string();
        }
        let host = tree.server.split(':').next().unwrap_or(&tree.server);
        if normalized.is_empty() {
            format!("{host}\\{}", tree.share_name)
        } else {
            format!("{host}\\{}\\{normalized}", tree.share_name)
        }
    }

    /// Run one operation under the backend's timeout. A timeout is connection trouble, i.e.
    /// `Transient` — never anything a caller could read as "the file is gone".
    fn block<F, T>(&self, what: &str, fut: F) -> VfsResult<T>
    where
        F: std::future::Future<Output = smb2::Result<T>>,
    {
        let d = self.timeout;
        // The timeout future has to be built inside the runtime (it takes the timer at
        // construction), hence the async block rather than a bare block_on(timeout(..)).
        match self.rt.block_on(async { tokio::time::timeout(d, fut).await }) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(map_smb_err(what, e)),
            Err(_) => Err(VfsError::new(
                VfsErrorKind::Transient,
                format!("{what} timed out after {d:?} on '{}'", self.spec.display()),
            )),
        }
    }

    /// `block` for a step that classifies its own failures (the hand-rolled compounds).
    fn block_vfs<F, T>(&self, what: &str, fut: F) -> VfsResult<T>
    where
        F: std::future::Future<Output = VfsResult<T>>,
    {
        let d = self.timeout;
        match self.rt.block_on(async { tokio::time::timeout(d, fut).await }) {
            Ok(r) => r,
            Err(_) => Err(VfsError::new(
                VfsErrorKind::Transient,
                format!("{what} timed out after {d:?} on '{}'", self.spec.display()),
            )),
        }
    }

    /// The FFS defense, same as the sftp backend: a not-found answer only becomes *confirmed*
    /// absence once the parent has been listed and really does not carry the name. Anything
    /// else — including a listing we cannot obtain — is `Transient`, because a transient fault
    /// reported as a deletion is the one failure mode this tool exists not to have.
    fn confirm_absent(&self, rel: &str) -> VfsResult<()> {
        let (parent, name) = crate::foundation::path::split_parent(rel);
        let parent = parent.trim_end_matches('/');
        match self.read_dir(parent) {
            Ok(entries) => {
                if entries.iter().any(|e| e.name == name) {
                    return Err(VfsError::new(
                        VfsErrorKind::Transient,
                        format!(
                            "the server reported '{rel}' missing but its parent still lists it — treating as a temporary fault, not a deletion"
                        ),
                    ));
                }
                Ok(())
            }
            // Parent gone too: the absence stands on its own.
            Err(e) if e.kind == VfsErrorKind::NotFound => Ok(()),
            Err(e) => Err(VfsError::new(
                VfsErrorKind::Transient,
                format!("cannot confirm '{rel}' is really absent (parent listing failed: {e})"),
            )),
        }
    }
}

/// Map the crate's errors onto the VFS taxonomy.
///
/// The asymmetry in `error.rs` is the point: everything ambiguous goes to `Transient`, and
/// only a server's own definite "no such name" becomes `NotFound` — which callers still
/// double-check before trusting.
fn map_smb_err(what: &str, e: smb2::Error) -> VfsError {
    use smb2::ErrorKind as K;
    if let smb2::Error::Protocol { status, .. } = &e {
        if status.0 == STATUS_DIRECTORY_NOT_EMPTY {
            return VfsError::new(
                VfsErrorKind::NotEmpty,
                format!("{what}: the directory is not empty"),
            );
        }
    }
    let kind = match e.kind() {
        K::NotFound => VfsErrorKind::NotFound,
        K::AlreadyExists => VfsErrorKind::AlreadyExists,
        K::AccessDenied => VfsErrorKind::PermissionDenied,
        K::AuthRequired | K::SigningRequired => VfsErrorKind::Auth,
        K::Cancelled => VfsErrorKind::Cancelled,
        // A held file, a dropped link and an expired session all clear on a retry, and none
        // of them says anything about whether the file exists.
        K::SharingViolation | K::ConnectionLost | K::TimedOut | K::SessionExpired => {
            VfsErrorKind::Transient
        }
        K::Io => VfsErrorKind::Io,
        // The server answered with a code — evidence the connection itself is fine.
        _ => VfsErrorKind::Protocol,
    };
    VfsError::new(kind, format!("{what}: {e}"))
}

/// A non-success status from one sub-response of a compound, routed through the same table.
fn status_err(what: &str, command: Command, status: NtStatus) -> VfsError {
    map_smb_err(what, smb2::Error::Protocol { status, command })
}

/// Set a file's write time and nothing else: compound CREATE + SET_INFO + CLOSE, one
/// round-trip, the shape the crate itself uses for `rename`.
///
/// This is the one `Vfs` operation the crate offers no high-level call for, and the one the
/// rest of the backend cannot do without — the compare window reads mtimes, and
/// `fs::lock::RootLock`'s heartbeat *is* a repeated `set_mtime`, so a backend that cannot do
/// this cannot hold a root lock. Shared by `Vfs::set_mtime` and the staged write's commit.
///
/// `wire_path` must already be normalized (see `SmbBackend::wire_path`).
/// `FileId::SENTINEL` is the compound's "the handle the previous operation just opened".
async fn set_write_time(
    tree: &Tree,
    conn: &Connection,
    wire_path: &str,
    ticks: u64,
) -> VfsResult<()> {
    let create = CreateRequest {
        requested_oplock_level: OplockLevel::None,
        impersonation_level: ImpersonationLevel::Impersonation,
        // FILE_WRITE_ATTRIBUTES is the whole reason this is hand-rolled: the crate's own
        // `open_file` asks for read access only, and a SET_INFO through such a handle comes
        // back ACCESS_DENIED.
        desired_access: FileAccessMask::new(
            FileAccessMask::FILE_WRITE_ATTRIBUTES | FileAccessMask::FILE_READ_ATTRIBUTES,
        ),
        file_attributes: 0,
        share_access: ShareAccess(
            ShareAccess::FILE_SHARE_READ
                | ShareAccess::FILE_SHARE_WRITE
                | ShareAccess::FILE_SHARE_DELETE,
        ),
        create_disposition: CreateDisposition::FileOpen,
        // Deliberately unconstrained: the lock heartbeat stamps a file and the engine also
        // stamps directories, so pinning FILE_NON_DIRECTORY_FILE either way would refuse half
        // the callers.
        create_options: 0,
        name: wire_path.to_string(),
        create_contexts: vec![],
    };
    let set = SetInfoRequest {
        info_type: InfoType::File,
        file_info_class: basic_info::FILE_BASIC_INFORMATION,
        additional_information: 0,
        file_id: FileId::SENTINEL,
        buffer: basic_info::set_write_time_buffer(ticks),
    };
    let close = CloseRequest { flags: 0, file_id: FileId::SENTINEL };
    let ops = [
        CompoundOp::new(Command::Create, &create, Some(tree.tree_id)),
        CompoundOp::new(Command::SetInfo, &set, Some(tree.tree_id)),
        CompoundOp::new(Command::Close, &close, Some(tree.tree_id)),
    ];

    let frames = conn
        .execute_compound(&ops)
        .await
        .map_err(|e| map_smb_err("set_mtime", e))?;
    let mut frames = frames.into_iter();
    let missing = |op: &str| {
        VfsError::new(
            VfsErrorKind::Transient,
            format!("set_mtime on '{wire_path}': the server answered nothing for the {op}"),
        )
    };

    let create_frame = frames
        .next()
        .ok_or_else(|| missing("CREATE"))?
        .map_err(|e| map_smb_err("set_mtime (create)", e))?;
    if create_frame.header.status != NtStatus::SUCCESS {
        // Everything after CREATE cascades, so its status is the one that explains the failure.
        return Err(status_err(
            &format!("set_mtime: cannot open '{wire_path}' to stamp it"),
            Command::Create,
            create_frame.header.status,
        ));
    }

    let set_frame = frames
        .next()
        .ok_or_else(|| missing("SET_INFO"))?
        .map_err(|e| map_smb_err("set_mtime (set_info)", e))?;
    if set_frame.header.status != NtStatus::SUCCESS {
        // The CLOSE cascaded with it, so the handle is still open on the server. Give it back
        // before reporting, or a failing lock heartbeat leaks a handle every beat.
        if let Ok(resp) = CreateResponse::unpack(&mut ReadCursor::new(&create_frame.body)) {
            let mut c = conn.clone();
            let _ = tree.close_handle(&mut c, resp.file_id).await;
        }
        return Err(status_err(
            &format!("set_mtime: the server refused the write time on '{wire_path}'"),
            Command::SetInfo,
            set_frame.header.status,
        ));
    }
    // The CLOSE's own status is not worth failing a landed stamp over: the time is set, and a
    // handle the server chose not to close is its own to reap.
    Ok(())
}

fn meta_of(size: u64, is_dir: bool, modified_ticks: u64) -> VMeta {
    VMeta {
        kind: if is_dir { EntryKind::Dir } else { EntryKind::File },
        size,
        mtime_ms: basic_info::unix_ms_from_filetime(modified_ticks),
        // SMB2 carries DOS attributes, not a unix mode; inventing 0o644 here would be a lie
        // the engine cannot tell from a real one.
        mode: None,
        file_id: None,
        link: None,
    }
}

impl Vfs for SmbBackend {
    fn caps(&self) -> VfsCaps {
        VfsCaps {
            protocol: "smb",
            // FILETIME is 100-ns ticks on the wire, so the protocol imposes no rounding worth
            // declaring. A coarser server (a FAT share behind the box) shows up as an mtime
            // correction through the CommitReport rather than a silently widened window.
            mtime_precision_ms: 1,
            set_mtime: Support::Yes, // compound CREATE + SET_INFO(FileBasicInformation) + CLOSE
            fsync: Support::Yes,     // the writer's finish() issues a server-side FLUSH first
            rename: Support::Yes,
            // FileRenameInformation goes out with ReplaceIfExists = 0, so an occupied target
            // is refused. The engine clears the destination itself; this records whose job it is.
            rename_overwrite: Support::No,
            ranged_read: Support::Yes, // positioned reads on one open handle
            write_at: Support::No,     // staged writes are sequential; delta is a both-local affair
            unix_mode: Support::No,
            symlink: Support::No, // reparse points are not the same thing, and guessing is worse
            file_id: Support::No,
            free_space: Support::Yes,
            read_back: Support::Yes,
            medium: Medium::NetworkShare,
            local_trash: false,
            case_sensitivity: CaseSense::Unknown,
            // Unlike the translation backend, nothing here passes through the local path
            // layer — the packets go straight out, so the rules that apply are the *server's*
            // and we cannot see its OS. `Unknown` downgrades the legality preflight from a
            // refusal to a visible warning, which is the honest outcome.
            name_rules: NameRules::Unknown,
            max_parallel_streams: 4,
        }
    }

    fn display(&self) -> String {
        self.spec.display()
    }

    fn identity(&self) -> String {
        self.spec.identity()
    }

    fn server_info(&self) -> Option<String> {
        self.server_line.lock().unwrap().clone()
    }

    fn connect(&self) -> VfsResult<()> {
        let _g = self.connect_gate.lock().unwrap();
        if self.conn.get().is_some() {
            return Ok(());
        }
        let user = self.spec.user.clone().ok_or_else(|| {
            VfsError::new(
                VfsErrorKind::Auth,
                format!(
                    "'{}' names no user — spell it smb://user@host/share (NTLM has no anonymous mode)",
                    self.spec.display()
                ),
            )
        })?;
        let creds = self.creds.credentials_for(&self.spec)?;
        let password = creds.password.clone().ok_or_else(|| {
            VfsError::new(
                VfsErrorKind::Auth,
                format!(
                    "no stored secret for '{}' — run: syncdash cred set \"{}\"\n\
                     (a native smb root always needs one: the client cannot use this machine's \
                     session login, unlike a plain \\\\host\\share path)",
                    self.spec.display(),
                    self.spec.display()
                ),
            )
        })?;
        let addr = format!("{}:{}", self.spec.host, self.spec.port.unwrap_or(445));
        let config = ClientConfig {
            addr: addr.clone(),
            timeout: self.timeout,
            username: user.clone(),
            password,
            domain: self.spec.opt("domain").unwrap_or_default().to_string(),
            auto_reconnect: false,
            compression: true,
            dfs_enabled: true,
            dfs_target_overrides: Default::default(),
        };

        let share = self.share.clone();
        let built = self.rt.block_on(async {
            let mut client = SmbClient::connect(config)
                .await
                .map_err(|e| map_smb_err("connect", e))?;
            let tree = client
                .connect_share(&share)
                .await
                .map_err(|e| map_smb_err(&format!("connect to share '{share}'"), e))?;
            let conn = client.connection_mut().clone();
            Ok::<_, VfsError>((client, conn, tree))
        })?;
        let (client, conn, tree) = built;

        let dialect = client
            .params()
            .map(|p| format!("{:?}", p.dialect))
            .unwrap_or_else(|| "unknown dialect".to_string());
        *self.server_line.lock().unwrap() =
            Some(format!("smb2 native: {addr} as {user}, {dialect}, share '{}'", self.share));
        let _ = self.conn.set(Conn { _client: client, conn, tree: Arc::new(tree) });
        Ok(())
    }

    // -------- read side --------

    fn stat(&self, rel: &str) -> VfsResult<Option<VMeta>> {
        let c = self.conn()?;
        let (tree, mut conn) = (c.tree.clone(), c.conn.clone());
        let p = self.share_rel(rel);
        match self.block("stat", async move { tree.stat(&mut conn, &p).await }) {
            Ok(i) => Ok(Some(meta_of(i.size, i.is_directory, i.modified.0))),
            Err(e) if e.kind == VfsErrorKind::NotFound => {
                if rel.is_empty() {
                    return Ok(None); // the root itself missing needs no parent check
                }
                self.confirm_absent(rel)?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn read_dir(&self, rel: &str) -> VfsResult<Vec<VDirEntry>> {
        let c = self.conn()?;
        let (tree, mut conn) = (c.tree.clone(), c.conn.clone());
        let p = self.share_rel(rel);
        let entries =
            self.block("read_dir", async move { tree.list_directory(&mut conn, &p).await })?;
        Ok(entries
            .into_iter()
            // The server sends the self and parent links; the table vocabulary has no room
            // for them and the crate passes them through untouched.
            .filter(|e| e.name != "." && e.name != "..")
            .map(|e| VDirEntry {
                meta: meta_of(e.size, e.is_directory, e.modified.0),
                name: e.name,
            })
            .collect())
    }

    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>> {
        let c = self.conn()?;
        let (tree, conn) = (c.tree.clone(), c.conn.clone());
        let p = self.share_rel(rel);
        let reader = self.block("open_read", async move {
            smb2::client::stream::open_file_reader(tree, conn, &p).await
        })?;
        Ok(Box::new(SmbRead::new(self.rt.handle().clone(), self.timeout, reader)))
    }

    fn read_range(&self, rel: &str, off: u64, len: u32) -> VfsResult<Vec<u8>> {
        let c = self.conn()?;
        let (tree, conn) = (c.tree.clone(), c.conn.clone());
        let p = self.share_rel(rel);
        self.block("read_range", async move {
            let reader = smb2::client::stream::open_file_reader(tree, conn, &p).await?;
            // READ answers short at end of file, which is exactly the contract's one
            // permitted shortfall, so the result passes straight through.
            let out = reader.read_at(off, len as u64).await;
            let closed = reader.close().await;
            let out = out?;
            closed?;
            Ok(out)
        })
    }

    fn read_link(&self, _rel: &str) -> VfsResult<String> {
        Err(VfsError::unsupported(
            "smb roots do not carry symlinks (caps().symlink says No; reparse points are a different thing and guessing at one would be worse)",
        ))
    }

    // -------- write side --------

    fn mkdir_all(&self, rel: &str) -> VfsResult<()> {
        let mut prefix = String::new();
        for seg in rel.split('/').filter(|s| !s.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(seg);
            let c = self.conn()?;
            let (tree, mut conn) = (c.tree.clone(), c.conn.clone());
            let p = self.share_rel(&prefix);
            match self.block("mkdir", async move { tree.create_directory(&mut conn, &p).await }) {
                Ok(()) => {}
                Err(e) => {
                    // "Already there" is the normal case on the way down an existing tree, but
                    // it is confirmed by looking rather than by trusting the status code.
                    match self.stat(&prefix)? {
                        Some(m) if m.kind == EntryKind::Dir => {}
                        _ => return Err(e),
                    }
                }
            }
        }
        Ok(())
    }

    fn open_write(&self, rel: &str, hint: &WriteHint) -> VfsResult<Box<dyn WriteStaged>> {
        let c = self.conn()?;
        let (parent, base) = crate::foundation::path::split_parent(rel);
        let tmp_rel = format!(
            "{parent}{}{base}.{}",
            crate::foundation::names::TEMP_PREFIX,
            std::process::id()
        );
        let tmp_share_rel = self.share_rel(&tmp_rel);

        // The staged open below does not truncate, so debris a previous interruption left
        // under this name would survive as a tail on the new file. Clear it first; absent is
        // the normal answer and not an error.
        {
            let (tree, mut conn) = (c.tree.clone(), c.conn.clone());
            let p = tmp_share_rel.clone();
            match self
                .block("clear stale temp", async move { tree.delete_file(&mut conn, &p).await })
            {
                Ok(()) => {}
                Err(e) if e.kind == VfsErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }

        let (tree, mut conn) = (c.tree.clone(), c.conn.clone());
        // `Tree::open_file*` are the crate's raw primitives: unlike `stat`, `rename`,
        // `delete_file` and the `stream::open_file_*` helpers, they put the path on the wire
        // exactly as given. A '/'-separated one gets STATUS_INVALID_PARAMETER, so the wire
        // form goes in here and only here.
        let p = self.wire_path(&tmp_rel);
        // Opened read+write with sharing allowed, which is what lets `staged_len` and the
        // read-back ask the server directly while the handle is still open.
        let (file_id, _) = self
            .block("open staged", async move { tree.open_file_readwrite(&mut conn, &p).await })?;
        let max_write = c.conn.params().map(|p| p.max_write_size).unwrap_or(65_536);
        Ok(Box::new(SmbStaged {
            rt: self.rt.handle().clone(),
            timeout: self.timeout,
            tree: c.tree.clone(),
            conn: c.conn.clone(),
            tmp_share_rel,
            dst_share_rel: self.share_rel(rel),
            dst_wire_path: self.wire_path(rel),
            max_write,
            file_id: Some(file_id),
            offset: 0,
            hint: hint.clone(),
            committed: false,
        }))
    }

    fn rename(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        let c = self.conn()?;
        let (tree, mut conn) = (c.tree.clone(), c.conn.clone());
        let (f, t) = (self.share_rel(from_rel), self.share_rel(to_rel));
        // ReplaceIfExists = 0 on the wire: an occupied target is refused, which is the
        // semantics caps() declares and the engine clears the destination for.
        self.block("rename", async move { tree.rename(&mut conn, &f, &t).await })
    }

    fn remove_file(&self, rel: &str) -> VfsResult<()> {
        let c = self.conn()?;
        let (tree, mut conn) = (c.tree.clone(), c.conn.clone());
        let p = self.share_rel(rel);
        self.block("remove_file", async move { tree.delete_file(&mut conn, &p).await })
    }

    /// Empty directories only — and the outcome is **verified, not trusted**.
    ///
    /// Measured against macOS 15's smbd: asked to delete a *populated* directory it answers
    /// STATUS_SUCCESS to both the CREATE(DELETE_ON_CLOSE) and the CLOSE, and then leaves the
    /// directory exactly where it was. It is not a recursive delete — the contents survive
    /// untouched — but it is not a deletion either, and taking the success at face value would
    /// report a removal that never happened, which the engine can never reconcile afterwards.
    /// So the answer is checked against the share rather than believed.
    fn remove_dir(&self, rel: &str) -> VfsResult<()> {
        let c = self.conn()?;
        let (tree, mut conn) = (c.tree.clone(), c.conn.clone());
        let p = self.share_rel(rel);
        self.block("remove_dir", async move { tree.delete_directory(&mut conn, &p).await })?;
        match self.stat(rel)? {
            None => Ok(()),
            Some(_) => {
                // Still there. A populated directory is the overwhelmingly common cause, and
                // the engine's delete-dir classification rides on that kind — but it is
                // established by looking, never by guessing.
                if self.read_dir_names(rel)?.is_empty() {
                    Err(VfsError::new(
                        VfsErrorKind::Protocol,
                        format!(
                            "the server reported '{rel}' removed and it is still there, though empty"
                        ),
                    ))
                } else {
                    Err(VfsError::new(
                        VfsErrorKind::NotEmpty,
                        format!("directory not empty: {rel}"),
                    ))
                }
            }
        }
    }

    /// The one `Vfs` method the crate has no high-level call for, and the one the rest of the
    /// backend cannot do without: the compare window reads mtimes, and `fs::lock::RootLock`'s
    /// heartbeat *is* a repeated `set_mtime`, so a backend that cannot do this cannot hold a
    /// root lock.
    ///
    /// The wire work is `set_write_time`, shared with the staged write's commit.
    fn set_mtime(&self, rel: &str, mtime_ms: i64) -> VfsResult<()> {
        let c = self.conn()?;
        let path = self.wire_path(rel);
        let ticks = basic_info::filetime_from_unix_ms(mtime_ms);
        self.block_vfs("set_mtime", set_write_time(&c.tree, &c.conn, &path, ticks))
    }

    fn set_mode(&self, _rel: &str, _mode: u32) -> VfsResult<()> {
        Err(VfsError::unsupported(
            "smb roots carry DOS attributes, not unix modes (caps().unix_mode says No)",
        ))
    }

    fn make_symlink(&self, _rel: &str, _target: &str) -> VfsResult<()> {
        Err(VfsError::unsupported(
            "smb roots do not take symlinks (caps().symlink says No)",
        ))
    }

    fn free_space(&self) -> VfsResult<Option<(u64, u64)>> {
        let c = self.conn()?;
        let (tree, mut conn) = (c.tree.clone(), c.conn.clone());
        let i = self.block("free_space", async move { tree.fs_info(&mut conn).await })?;
        Ok(Some((i.free_bytes, i.total_bytes)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::spec::{parse, RootSpec};

    fn backend(s: &str) -> SmbBackend {
        let RootSpec::Remote(r) = parse(s) else { panic!() };
        SmbBackend::new(r, crate::fs::vfs::cred::default_provider()).unwrap()
    }

    #[test]
    fn share_and_sub_split() {
        let b = backend("smb://ben@server/photos/2026/07");
        assert_eq!(b.share, "photos");
        assert_eq!(b.sub, "2026/07");
        let b2 = backend("smb://server/backup");
        assert_eq!(b2.share, "backup");
        assert_eq!(b2.sub, "");
    }

    #[test]
    fn no_share_is_refused_at_construction() {
        let RootSpec::Remote(r) = parse("smb://server") else { panic!() };
        assert!(SmbBackend::new(r, crate::fs::vfs::cred::default_provider()).is_err());
    }

    /// The share is the tree, so it must never reappear inside the path; the phrase's
    /// subdirectory must.
    #[test]
    fn paths_resolve_under_the_subdirectory_not_the_share() {
        let b = backend("smb://ben@server/photos/2026/07");
        assert_eq!(b.share_rel("a/b.txt"), "2026/07/a/b.txt");
        assert_eq!(b.share_rel(""), "2026/07");
        let flat = backend("smb://ben@server/photos");
        assert_eq!(flat.share_rel("a/b.txt"), "a/b.txt");
        assert_eq!(flat.share_rel(""), "");
    }

    /// What actually goes on the wire: backslashes, and no leading separator for the server
    /// to read as an absolute path.
    #[test]
    fn wire_paths_are_backslashed_and_relative() {
        let b = backend("smb://ben@server/photos/2026");
        assert_eq!(b.wire_path("a/b.txt"), "2026\\a\\b.txt");
        assert_eq!(b.wire_path(""), "2026");
        let flat = backend("smb://ben@server/photos");
        assert_eq!(flat.wire_path(""), "", "the share root is the empty path, not '\\'");
        assert_eq!(flat.wire_path("x.txt"), "x.txt");
    }

    #[test]
    fn unconnected_backend_reports_transient_not_missing() {
        let b = backend("smb://ben@server/share");
        let e = b.stat("x").unwrap_err();
        assert_eq!(e.kind, VfsErrorKind::Transient);
    }

    #[test]
    fn write_side_still_needs_a_connection_first() {
        let b = backend("smb://ben@server/share");
        let e = b.set_mtime("x", 0).unwrap_err();
        assert_eq!(
            e.kind,
            VfsErrorKind::Transient,
            "unconnected is a transient state, never a judgment about files"
        );
    }

    #[test]
    fn caps_declare_the_smb_profile() {
        let c = backend("smb://ben@server/share").caps();
        assert_eq!(c.protocol, "smb");
        assert!(c.set_mtime.yes(), "the whole point of this backend");
        assert_eq!(c.rename_overwrite, Support::No, "ReplaceIfExists goes out as 0");
        assert_eq!(c.symlink, Support::No);
        assert_eq!(c.unix_mode, Support::No);
        assert!(c.ranged_read.yes());
        assert!(!c.local_trash);
        // Nothing passes through the local path layer, so the client's rules do not apply.
        assert_eq!(c.name_rules, NameRules::Unknown);
    }

    /// The capability map and the run-time answers have to agree: a `No` must come back as
    /// `Unsupported`, not as a silent success or a protocol error.
    #[test]
    fn declared_gaps_refuse_honestly() {
        let b = backend("smb://ben@server/share");
        assert_eq!(b.set_mode("x", 0o644).unwrap_err().kind, VfsErrorKind::Unsupported);
        assert_eq!(b.make_symlink("x", "y").unwrap_err().kind, VfsErrorKind::Unsupported);
        assert_eq!(b.read_link("x").unwrap_err().kind, VfsErrorKind::Unsupported);
    }

    /// The engine's delete-dir classification rides on this one code, which the crate folds
    /// into a generic `Other`.
    #[test]
    fn directory_not_empty_is_classified_not_lumped_into_protocol() {
        let e = map_smb_err(
            "remove_dir",
            smb2::Error::Protocol {
                status: NtStatus(STATUS_DIRECTORY_NOT_EMPTY),
                command: Command::Create,
            },
        );
        assert_eq!(e.kind, VfsErrorKind::NotEmpty);
    }

    /// A held file and a dropped link must never read as "the file is gone" — that asymmetry
    /// is the difference between an annoyance and a reverse delete.
    #[test]
    fn ambiguous_failures_stay_transient() {
        for status in [NtStatus::SHARING_VIOLATION, NtStatus::ACCESS_DENIED] {
            let e = map_smb_err("op", smb2::Error::Protocol { status, command: Command::Create });
            assert_ne!(e.kind, VfsErrorKind::NotFound, "{status} must not read as absence");
        }
        assert_eq!(map_smb_err("op", smb2::Error::Disconnected).kind, VfsErrorKind::Transient);
        assert_eq!(map_smb_err("op", smb2::Error::Timeout).kind, VfsErrorKind::Transient);
    }

    #[test]
    fn a_missing_name_is_the_only_thing_that_becomes_not_found() {
        let e = map_smb_err(
            "stat",
            smb2::Error::Protocol {
                status: NtStatus::OBJECT_NAME_NOT_FOUND,
                command: Command::Create,
            },
        );
        assert_eq!(e.kind, VfsErrorKind::NotFound);
    }
}
