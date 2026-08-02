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
//! the crate has no high-level call for. Lease ownership itself uses immutable no-replace claims;
//! mtime only keeps their owner-specific heartbeat record observable.

mod basic_info;
mod errors;
mod meta;
mod session;
mod staged;

#[cfg(test)]
mod tests;

pub use session::SmbBackend;

use self::errors::{map_smb_err, status_err};
use self::meta::meta_of;
use self::session::Conn;
use self::staged::{SmbRead, SmbStaged};
use super::error::{VfsError, VfsErrorKind, VfsResult};
use super::VfsEntryKind;
use super::{
    CaseSense, Medium, NameRules, ReadStream, Support, VDirEntry, VMeta, Vfs, VfsCaps, WriteHint,
    WriteStaged,
};
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
use std::sync::Arc;

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
    let close = CloseRequest {
        flags: 0,
        file_id: FileId::SENTINEL,
    };
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
            exclusive_staged_file_publish: Support::Yes,
            exclusive_entry_rename: Support::Yes,
            exclusive_symlink_publish: Support::No,
            durable_namespace: Support::Unknown,
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
            // Names go on the wire as spelled, so what governs them is the *server's* rules and
            // its OS is not visible from here. `Unknown` downgrades the legality preflight from
            // a refusal to a visible warning, which is the honest outcome.
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
        *self.server_line.lock().unwrap() = Some(format!(
            "smb2 native: {addr} as {user}, {dialect}, share '{}'",
            self.share
        ));
        let _ = self.conn.set(Conn {
            _client: client,
            conn,
            tree: Arc::new(tree),
        });
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
        let entries = self.block("read_dir", async move {
            tree.list_directory(&mut conn, &p).await
        })?;
        let mut out = Vec::new();
        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            let name = crate::foundation::path::EntryName::try_from(entry.name).map_err(|e| {
                VfsError::new(
                    VfsErrorKind::Protocol,
                    format!("SMB server returned an invalid directory entry: {e}"),
                )
            })?;
            out.push(VDirEntry {
                meta: meta_of(entry.size, entry.is_directory, entry.modified.0),
                name,
            });
        }
        Ok(out)
    }

    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>> {
        let c = self.conn()?;
        let (tree, conn) = (c.tree.clone(), c.conn.clone());
        let p = self.share_rel(rel);
        let reader = self.block("open_read", async move {
            smb2::client::stream::open_file_reader(tree, conn, &p).await
        })?;
        Ok(Box::new(SmbRead::new(
            self.rt.handle().clone(),
            self.timeout,
            reader,
        )))
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
            match self.block("mkdir", async move {
                tree.create_directory(&mut conn, &p).await
            }) {
                Ok(()) => {}
                Err(e) => {
                    // "Already there" is the normal case on the way down an existing tree, but
                    // it is confirmed by looking rather than by trusting the status code.
                    match self.stat(&prefix)? {
                        Some(m) if m.kind == VfsEntryKind::Directory => {}
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
        let token = super::random_name_token()?;
        let tmp_rel = format!(
            "{parent}{}{base}.{token}",
            crate::foundation::names::TEMP_PREFIX,
        );
        let tmp_share_rel = self.share_rel(&tmp_rel);

        let (tree, conn) = (c.tree.clone(), c.conn.clone());
        // `Tree::open_file*` are the crate's raw primitives: unlike `stat`, `rename`,
        // `delete_file` and the `stream::open_file_*` helpers, they put the path on the wire
        // exactly as given. A '/'-separated one gets STATUS_INVALID_PARAMETER, so the wire
        // form goes in here and only here.
        let p = self.wire_path(&tmp_rel);
        let (file_id, _) = self.block_vfs("open staged", async move {
            let request = CreateRequest {
                requested_oplock_level: OplockLevel::None,
                impersonation_level: ImpersonationLevel::Impersonation,
                desired_access: FileAccessMask::new(
                    FileAccessMask::FILE_READ_DATA
                        | FileAccessMask::FILE_WRITE_DATA
                        | FileAccessMask::FILE_READ_ATTRIBUTES
                        | FileAccessMask::FILE_WRITE_ATTRIBUTES
                        | FileAccessMask::SYNCHRONIZE,
                ),
                file_attributes: 0x80,
                share_access: ShareAccess(
                    ShareAccess::FILE_SHARE_READ
                        | ShareAccess::FILE_SHARE_WRITE
                        | ShareAccess::FILE_SHARE_DELETE,
                ),
                create_disposition: CreateDisposition::FileCreate,
                create_options: 0x0000_0040,
                name: p,
                create_contexts: vec![],
            };
            let frame = conn
                .execute(Command::Create, &request, Some(tree.tree_id))
                .await
                .map_err(|e| map_smb_err("open staged", e))?;
            if frame.header.status != NtStatus::SUCCESS {
                return Err(status_err(
                    "open staged",
                    Command::Create,
                    frame.header.status,
                ));
            }
            let response = CreateResponse::unpack(&mut ReadCursor::new(&frame.body))
                .map_err(|e| map_smb_err("open staged response", e))?;
            Ok((response.file_id, response.end_of_file))
        })?;
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
        self.block(
            "rename",
            async move { tree.rename(&mut conn, &f, &t).await },
        )
    }

    fn remove_file(&self, rel: &str) -> VfsResult<()> {
        let c = self.conn()?;
        let (tree, mut conn) = (c.tree.clone(), c.conn.clone());
        let p = self.share_rel(rel);
        self.block("remove_file", async move {
            tree.delete_file(&mut conn, &p).await
        })
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
        self.block("remove_dir", async move {
            tree.delete_directory(&mut conn, &p).await
        })?;
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

    /// The one `Vfs` method the crate has no high-level call for. The compare window reads mtimes,
    /// staged commits preserve them, and a root lease uses this only to refresh its observable
    /// owner heartbeat; lease ownership comes from immutable no-replace claims.
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
