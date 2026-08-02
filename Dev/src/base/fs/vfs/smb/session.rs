//! Connecting to a share and keeping the session alive.
//!
//! The session is established once behind a gate and reused. The field order in SmbBackend is
//! load-bearing: struct fields drop in declaration order, and the session's teardown aborts a task
//! belonging to the runtime declared after it.

use super::super::error::{VfsError, VfsErrorKind, VfsResult};
use super::super::spec::EndpointSpec;
use super::super::{CredentialProvider, Vfs};
use super::errors::map_smb_err;
use smb2::client::connection::Connection;
use smb2::{SmbClient, Tree};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub struct SmbBackend {
    pub(super) spec: EndpointSpec,
    pub(super) creds: Arc<dyn CredentialProvider>,
    /// Share name (first segment of the phrase root); the tree is connected to this.
    pub(super) share: String,
    /// Path below the share named by the phrase. Every `rel` is resolved under it.
    pub(super) sub: String,
    pub(super) timeout: Duration,
    /// Declared ahead of `rt` deliberately: struct fields drop in declaration order, and the
    /// session's teardown aborts a task belonging to that runtime. Dropping the runtime first
    /// would leave it reaching into an executor that no longer exists.
    pub(super) conn: OnceLock<Conn>,
    pub(super) rt: tokio::runtime::Runtime,
    pub(super) connect_gate: Mutex<()>,
    pub(super) server_line: Mutex<Option<String>>,
}

pub(super) struct Conn {
    /// Held only to keep the session and its receiver task alive; every operation runs on a
    /// clone of `conn` instead, because the client's own methods want `&mut self` and would
    /// serialize the backend behind one lock.
    pub(super) _client: SmbClient,
    pub(super) conn: Connection,
    pub(super) tree: Arc<Tree>,
}

impl SmbBackend {
    pub fn new(spec: EndpointSpec, creds: Arc<dyn CredentialProvider>) -> VfsResult<SmbBackend> {
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

    pub(super) fn conn(&self) -> VfsResult<&Conn> {
        self.conn.get().ok_or_else(|| {
            VfsError::new(
                VfsErrorKind::Transient,
                format!(
                    "'{}' is not connected — connect() must run first",
                    self.spec.display()
                ),
            )
        })
    }

    /// A table rel resolved against the phrase's own subdirectory, still '/'-separated.
    ///
    /// This is what the crate's own methods want: they run `format_path` over whatever they
    /// are given. Only the hand-rolled compound in `set_mtime` needs `wire_path` instead.
    pub(super) fn share_rel(&self, rel: &str) -> String {
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
    pub(super) fn wire_path(&self, rel: &str) -> String {
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
    pub(super) fn block<F, T>(&self, what: &str, fut: F) -> VfsResult<T>
    where
        F: std::future::Future<Output = smb2::Result<T>>,
    {
        let d = self.timeout;
        // The timeout future has to be built inside the runtime (it takes the timer at
        // construction), hence the async block rather than a bare block_on(timeout(..)).
        match self
            .rt
            .block_on(async { tokio::time::timeout(d, fut).await })
        {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(map_smb_err(what, e)),
            Err(_) => Err(VfsError::new(
                VfsErrorKind::Transient,
                format!("{what} timed out after {d:?} on '{}'", self.spec.display()),
            )),
        }
    }

    /// `block` for a step that classifies its own failures (the hand-rolled compounds).
    pub(super) fn block_vfs<F, T>(&self, what: &str, fut: F) -> VfsResult<T>
    where
        F: std::future::Future<Output = VfsResult<T>>,
    {
        let d = self.timeout;
        match self
            .rt
            .block_on(async { tokio::time::timeout(d, fut).await })
        {
            Ok(r) => r,
            Err(_) => Err(VfsError::new(
                VfsErrorKind::Transient,
                format!("{what} timed out after {d:?} on '{}'", self.spec.display()),
            )),
        }
    }

    /// Absence is confirmed by [`super::super::absence::confirm_absent`]; this backend supplies only
    /// its own parent listing.
    pub(super) fn confirm_absent(&self, rel: &str) -> VfsResult<()> {
        super::super::absence::confirm_absent(rel, |parent| {
            self.read_dir(parent).map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| entry.name.as_str().to_owned())
                    .collect()
            })
        })
    }
}
