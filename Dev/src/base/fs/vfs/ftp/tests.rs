use super::super::error::VfsErrorKind;
use super::super::{Support, Vfs};
use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::spec::{parse, RootSpec};

    fn backend(s: &str) -> FtpBackend {
        let RootSpec::Endpoint(r) = parse(s) else {
            panic!()
        };
        FtpBackend::new(r, crate::fs::vfs::cred::default_provider())
    }

    #[test]
    fn caps_before_connect_assume_the_worst_honestly() {
        let b = backend("ftp://anonymous@nas/pub");
        let c = b.caps();
        assert_eq!(c.mtime_precision_ms, 60_000);
        assert_eq!(c.set_mtime, Support::Unknown);
        assert_eq!(c.fsync, Support::No);
        assert_eq!(c.max_parallel_streams, 1);
    }

    #[test]
    fn abs_paths_are_rooted() {
        let b = backend("ftp://u@h/pub/data");
        assert_eq!(b.abs(""), "/pub/data");
        assert_eq!(b.abs("x/y.txt"), "/pub/data/x/y.txt");
        let r = backend("ftp://u@h");
        assert_eq!(r.abs(""), "/");
        assert_eq!(r.abs("a.txt"), "/a.txt");
    }

    /// `ftps://` used to refuse outright — the root store was undecided and a half-secure default
    /// is worse than an honest no. Now it is a real scheme, so the thing to pin is that it goes
    /// down the *same* path as `ftp://` rather than a special one: a phrase with no stored secret
    /// fails on credentials, not on the scheme.
    #[test]
    fn ftps_is_a_real_scheme_and_authenticates_like_ftp() {
        let e = backend("ftps://u@h/x").connect().unwrap_err();
        assert_eq!(
            e.kind,
            VfsErrorKind::Auth,
            "no stored secret — same rung ftp:// stops at"
        );
        assert_ne!(
            e.kind,
            VfsErrorKind::Unsupported,
            "the scheme itself is supported now"
        );

        // And it is still refused when the phrase names nobody, for the same reason ftp:// is:
        // anonymous has to be asked for, never assumed.
        let e = backend("ftps://h/x").connect().unwrap_err();
        assert_eq!(e.kind, VfsErrorKind::Auth);
    }

    /// The trust store has to actually yield roots on this machine, or every `ftps://` connection
    /// fails at verification for a reason that looks like the server's fault.
    #[test]
    fn the_os_trust_store_yields_usable_roots() {
        assert!(
            tls_connector().is_ok(),
            "no roots readable from this machine's store — ftps:// could not verify anything"
        );
    }
}
