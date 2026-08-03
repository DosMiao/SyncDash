//! Root phrase parsing: one string names where a sync root lives.
//!
//! Grammar (FFS path-phrase shape, minus its credential-in-string parts):
//!
//! ```text
//! endpoint := scheme "://" [user "@"] host [":" port] ["/" path] {"|" opt}
//! scheme := "sftp" | "ftp" | "ftps" | "smb" | "peer"   (case-insensitive)
//! host   := name | ipv4 | "[" ipv6 "]"
//! opt    := key ["=" value]
//! ```
//!
//! `peer://` is the odd one: it does not name a protocol this process speaks, it names a machine
//! running its own syncdash. It lives in the same grammar anyway because it answers the same
//! question — *where does this root live* — and that question is supposed to have one answer in
//! one place.
//!
//! Everything that is not a known scheme is a local path — with one deliberate
//! exception: `xyz://` with an *unknown* scheme parses to `UnknownScheme` instead of
//! falling through to local. Treating a typo like `sfpt://host/x` as a relative local
//! path would silently create a directory literally named `sfpt:` — the class of quiet
//! wrong-turn this project's no-silent rule exists to prevent. The error is raised by
//! config validation / `open()`, not here: parsing never fails, so a half-typed phrase
//! in an editor field stays representable.
//!
//! Credentials never appear in a phrase. Secrets live in the OS credential store.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootSpec {
    Local(PathBuf),
    Endpoint(EndpointSpec),
    /// `scheme://…` where the scheme is not one we know. Never silently local.
    UnknownScheme {
        raw: String,
        scheme: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointSpec {
    /// Lower-cased: "sftp" | "ftp" | "ftps" | "smb" | "peer"
    pub scheme: String,
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    /// Server-side root path, '/'-separated, no leading or trailing '/'. Empty = the
    /// protocol's default root (SFTP: the login home; SMB: the share root).
    pub root: String,
    /// The `|key=value` tail. Flag options carry an empty value, and interpretation is the
    /// backend's business. Parsing stays permissive — an option this build does not know is
    /// carried, not refused — so the list of what is actually *read* belongs here and has to stay
    /// honest:
    ///
    /// - `timeout=` — sftp, smb, ftp
    /// - `key=` — sftp, via `cred`
    /// - `domain=` — smb
    /// - `active` — ftp
    /// - `exe=`, `mount=` — `peer://`
    ///
    /// `agent` parses into `Credentials.use_agent` and **nothing reads it yet**: ssh-agent auth is
    /// not implemented, so the phrase silently falls through to the key/password chain. Named here
    /// rather than dropped because the parse would otherwise look like support.
    ///
    /// This list previously also advertised `insecure_tls`, `compress`, `max_conns=` and
    /// `no_sample`, none of which any backend has ever read. `insecure_tls` was the harmful one:
    /// `ftp::tls_connector` deliberately offers no way to skip certificate verification, so the
    /// grammar was promising an escape hatch the design exists to refuse.
    pub options: Vec<(String, String)>,
}

impl EndpointSpec {
    /// Display form: credentials-free by construction, options dropped.
    /// Goes into table headers, plan headers, logs and the UI.
    pub fn display(&self) -> String {
        let mut s = format!("{}://", self.scheme);
        if let Some(u) = &self.user {
            s.push_str(u);
            s.push('@');
        }
        push_host(&mut s, &self.host);
        if let Some(p) = self.port {
            s.push_str(&format!(":{p}"));
        }
        if !self.root.is_empty() {
            s.push('/');
            s.push_str(&self.root);
        }
        s
    }

    /// Canonical identity: cache keys, the mtime-correction table and lock ownership
    /// hang off this. Host is folded to lowercase (DNS is case-insensitive), the user
    /// is kept as-is (usernames are not), the port is always explicit so a default
    /// and a spelled-out default land on the same key.
    pub fn identity(&self) -> String {
        let mut s = format!("{}://", self.scheme);
        if let Some(u) = &self.user {
            s.push_str(u);
            s.push('@');
        }
        push_host(&mut s, &self.host.to_lowercase());
        s.push_str(&format!(
            ":{}",
            self.port.unwrap_or(default_port(&self.scheme))
        ));
        s.push('/');
        s.push_str(&self.root);
        s
    }

    pub fn opt(&self, key: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn has_flag(&self, key: &str) -> bool {
        self.options.iter().any(|(k, _)| k == key)
    }

    /// How long one operation may take before it counts as connection trouble. Every protocol
    /// backend reads the same `timeout=` option and shares the same default, so the phrase means
    /// one thing regardless of which backend the root routes to.
    pub fn timeout(&self) -> std::time::Duration {
        self.opt("timeout")
            .and_then(|t| t.parse::<u64>().ok())
            .map(std::time::Duration::from_secs)
            .unwrap_or(std::time::Duration::from_secs(20))
    }
}

fn push_host(s: &mut String, host: &str) {
    // IPv6 literals go back into brackets so the string stays re-parseable
    if host.contains(':') {
        s.push('[');
        s.push_str(host);
        s.push(']');
    } else {
        s.push_str(host);
    }
}

pub fn default_port(scheme: &str) -> u16 {
    match scheme {
        "sftp" | "peer" => 22,
        "ftp" | "ftps" => 21,
        "smb" => 445,
        _ => 0,
    }
}

pub const KNOWN_SCHEMES: &[&str] = &["sftp", "ftp", "ftps", "smb", "peer"];

/// Whether this scheme names a root a **peer syncdash** owns rather than one this process reaches.
///
/// The distinction is the transport, not the protocol: `sftp://` also travels over ssh, but this
/// process does the reading and writing. `peer://` means the far side scans its own disk and
/// applies a package against it.
pub fn is_peer_scheme(scheme: &str) -> bool {
    scheme == "peer"
}

/// Whether a root phrase names a peer root. The one place this question is asked.
///
/// It used to be asked of a `remote_host` field on the job instead — a second, unrelated place to
/// say where a root lives, which `validate_roots` then had to keep from contradicting the phrase.
pub fn is_peer(phrase: &str) -> bool {
    matches!(parse(phrase), RootSpec::Endpoint(r) if is_peer_scheme(&r.scheme))
}

/// Parse a root phrase. Never fails and never panics: anything unparseable in the
/// endpoint grammar still comes back as a representable value, and the error (if it is
/// one) is raised where it can carry context — config validation or `open()`.
pub fn parse(raw: &str) -> RootSpec {
    let s = raw.trim();

    // Local fast-paths first. A Windows drive letter contains ':' but can never
    // contain "://" at position 1, and UNC / \\?\ prefixes are unambiguous.
    if s.starts_with("\\\\") || looks_like_drive_path(s) {
        return RootSpec::Local(PathBuf::from(s));
    }

    if let Some(idx) = s.find("://") {
        let scheme_raw = &s[..idx];
        let scheme = scheme_raw.to_ascii_lowercase();
        if !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric()) {
            if KNOWN_SCHEMES.contains(&scheme.as_str()) {
                return RootSpec::Endpoint(parse_endpoint(&scheme, &s[idx + 3..]));
            }
            return RootSpec::UnknownScheme {
                raw: s.to_string(),
                scheme,
            };
        }
    }

    RootSpec::Local(PathBuf::from(s))
}

fn looks_like_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/')
}

/// `rest` is everything after `scheme://`.
fn parse_endpoint(scheme: &str, rest: &str) -> EndpointSpec {
    // Options tail: split on '|' first so a '|' can never be mistaken for path content
    let mut parts = rest.split('|');
    let head = parts.next().unwrap_or("");
    let options: Vec<(String, String)> = parts
        .filter(|p| !p.trim().is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
            None => (p.trim().to_string(), String::new()),
        })
        .collect();

    // authority [/ path]
    let (authority, path) = match head.find('/') {
        Some(i) => (&head[..i], &head[i + 1..]),
        None => (head, ""),
    };

    // [user@]host[:port] — split on the LAST '@' so a user name containing '@'
    // (rare but legal once percent-encoded) does not shift the host
    let (user, hostport) = match authority.rfind('@') {
        Some(i) => (Some(decode_user(&authority[..i])), &authority[i + 1..]),
        None => (None, authority),
    };

    let (host, port) = parse_hostport(hostport);

    EndpointSpec {
        scheme: scheme.to_string(),
        user,
        host,
        port,
        root: normalize_root(path),
        options,
    }
}

/// The three characters the grammar cannot carry raw in a user name (FFS's exact set).
fn decode_user(u: &str) -> String {
    u.replace("%40", "@")
        .replace("%3A", ":")
        .replace("%3a", ":")
        .replace("%25", "%")
}

fn parse_hostport(s: &str) -> (String, Option<u16>) {
    if let Some(rest) = s.strip_prefix('[') {
        // [ipv6]:port or [ipv6]
        if let Some(end) = rest.find(']') {
            let host = rest[..end].to_string();
            let after = &rest[end + 1..];
            let port = after.strip_prefix(':').and_then(|p| p.parse().ok());
            return (host, port);
        }
        return (rest.to_string(), None); // unclosed bracket: keep what we can
    }
    // A bare IPv6 (multiple ':') has no port syntax; a single ':' splits host:port
    if s.matches(':').count() == 1 {
        if let Some((h, p)) = s.split_once(':') {
            if let Ok(port) = p.parse() {
                return (h.to_string(), Some(port));
            }
        }
    }
    (s.to_string(), None)
}

/// Unify separators, strip leading/trailing '/' — the stored root joins onto rels
/// with a '/' between, same contract as `foundation::path`.
fn normalize_root(p: &str) -> String {
    let unified = p.replace('\\', "/");
    let mut segs: Vec<&str> = unified.split('/').filter(|s| !s.is_empty()).collect();
    // "." segments carry no meaning here
    segs.retain(|s| *s != ".");
    segs.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(s: &str) -> EndpointSpec {
        match parse(s) {
            RootSpec::Endpoint(r) => r,
            other => panic!("{s} parsed as {other:?}"),
        }
    }

    #[test]
    fn local_shapes_stay_local() {
        assert!(matches!(parse(r"D:\Code\x"), RootSpec::Local(_)));
        assert!(matches!(parse("D:/Code/x"), RootSpec::Local(_)));
        assert!(matches!(parse(r"\\server\share\dir"), RootSpec::Local(_)));
        assert!(matches!(parse(r"\\?\D:\very\long"), RootSpec::Local(_)));
        assert!(matches!(parse("/Users/x/Code"), RootSpec::Local(_)));
        assert!(matches!(parse("relative/dir"), RootSpec::Local(_)));
    }

    #[test]
    fn sftp_full_form() {
        let r =
            endpoint("sftp://ben@mac-mini:2222/Users/ben/Code|key=~/.ssh/id_ed25519|timeout=30");
        assert_eq!(r.scheme, "sftp");
        assert_eq!(r.user.as_deref(), Some("ben"));
        assert_eq!(r.host, "mac-mini");
        assert_eq!(r.port, Some(2222));
        assert_eq!(r.root, "Users/ben/Code");
        assert_eq!(r.opt("key"), Some("~/.ssh/id_ed25519"));
        assert_eq!(r.opt("timeout"), Some("30"));
        assert!(!r.has_flag("agent"));
    }

    #[test]
    fn minimal_and_flag_options() {
        let r = endpoint("ftp://nas/pub|active");
        assert_eq!(r.user, None);
        assert_eq!(r.host, "nas");
        assert_eq!(r.port, None);
        assert_eq!(r.root, "pub");
        assert!(r.has_flag("active"));
    }

    #[test]
    fn ipv6_forms() {
        let r = endpoint("sftp://ben@[fe80::1]:2222/data");
        assert_eq!(r.host, "fe80::1");
        assert_eq!(r.port, Some(2222));
        let r2 = endpoint("sftp://ben@[::1]/data");
        assert_eq!(r2.host, "::1");
        assert_eq!(r2.port, None);
        // display puts brackets back so the output re-parses
        assert_eq!(r.display(), "sftp://ben@[fe80::1]:2222/data");
    }

    #[test]
    fn unknown_scheme_never_silently_local() {
        for (phrase, expected) in [
            ("sfpt://host/x", "sfpt"),
            ("fake://seed1", "fake"),
            ("nfs://server/export", "nfs"),
        ] {
            match parse(phrase) {
                RootSpec::UnknownScheme { scheme, .. } => assert_eq!(scheme, expected),
                other => panic!("unknown scheme must be caught, got {other:?}"),
            }
        }
    }

    #[test]
    fn identity_folds_host_and_pins_port() {
        let a = endpoint("sftp://ben@Mac-Mini/Code");
        let b = endpoint("sftp://ben@mac-mini:22/Code/");
        assert_eq!(a.identity(), b.identity());
        // but the user stays case-sensitive
        let c = endpoint("sftp://Ben@mac-mini/Code");
        assert_ne!(a.identity(), c.identity());
    }

    #[test]
    fn display_omits_options() {
        let r = endpoint("smb://ben@server/share/photos|max_conns=2");
        assert_eq!(r.display(), "smb://ben@server/share/photos");
    }

    #[test]
    fn root_normalization() {
        assert_eq!(endpoint("smb://s/share\\sub\\dir").root, "share/sub/dir");
        assert_eq!(endpoint("sftp://h//a//b/").root, "a/b");
        assert_eq!(endpoint("sftp://h").root, "");
    }

    #[test]
    fn encoded_user_decodes() {
        let r = endpoint("sftp://user%40corp@host/x");
        assert_eq!(r.user.as_deref(), Some("user@corp"));
    }

    #[test]
    fn a_peer_phrase_carries_the_whole_link() {
        let r = endpoint(r"peer://mac/Users/ben/Code|exe=~/bin/syncdash|mount=\\mac\share\Code");
        assert_eq!(r.scheme, "peer");
        assert_eq!(r.host, "mac");
        assert_eq!(r.root, "Users/ben/Code");
        assert_eq!(r.opt("exe"), Some("~/bin/syncdash"));
        // The mount is a Windows path; splitting on '|' before anything else keeps it intact
        assert_eq!(r.opt("mount"), Some(r"\\mac\share\Code"));
        // …and no option ever reaches a display string
        assert_eq!(r.display(), "peer://mac/Users/ben/Code");
    }

    #[test]
    fn peer_is_recognized_only_by_its_own_scheme() {
        assert!(is_peer("peer://mac/Users/ben/x"));
        assert!(is_peer_scheme("peer"));
        // sftp also rides ssh, but there this process does the reading and writing
        assert!(!is_peer("sftp://ben@mac/Users/ben/x"));
        assert!(!is_peer(r"D:\Code\x"));
        assert!(!is_peer(r"\\mac\share\x"));
        // The default port matters: a spelled-out :22 and an implicit one must share a key
        assert_eq!(
            endpoint("peer://mac/x").identity(),
            endpoint("peer://mac:22/x/").identity()
        );
    }
}
