//! One `#[test]` per backend shape, offline and live.
//!
//! A lane is a factory for fresh empty roots plus the assertion that its **skip set** is what the
//! backend's declared capabilities imply. That set is the contract: a backend that quietly loses a
//! capability changes it, and the lane fails rather than silently testing less.
//!
//! Live lanes are `#[ignore]` behind an env var, and each holds its backend to *both* contracts off
//! the one factory — `fs::vfs::conformance` for the trait surface, this suite for the pipeline on
//! top of it. That pairing is the point: what runs by default for the protocol backends is `MemVfs`
//! wearing their declared capabilities, which can only ever prove the pipeline copes with that
//! shape — never that the backend has it.

use std::sync::Arc;

use super::*;
use crate::fs::vfs::conformance;
use crate::fs::vfs::local::LocalVfs;
use crate::fs::vfs::memory::MemVfs;
use crate::fs::vfs::{Support, Vfs};

/// The generic VFS lane — `as_local()` is `None`, so this drives the same `scan_vfs` path every
/// protocol backend rides, with no server involved.
#[test]
fn memory_lane_syncs() {
    let mut n = 0;
    let rep = run_all("memory", &mut || {
        n += 1;
        Arc::new(MemVfs::new(&format!("e2e-{n}"))) as Arc<dyn Vfs>
    });
    assert!(
        rep.skipped.is_empty(),
        "the memory backend declares every capability, so it should skip nothing: {:?}",
        rep.skipped
    );
    assert!(!rep.ran.is_empty(), "a lane that ran no cases is not a passing lane");
}

/// The real filesystem, and the only lane where `as_local()` is `Some` — so it is the only one that
/// exercises the walkdir/mmap fast path, the central trash route, and real NTFS timestamps.
/// Everything the memory lane proves about semantics, this proves about a disk.
#[test]
fn local_lane_syncs() {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut n = 0usize;
    let rep = {
        let mut mk = || {
            n += 1;
            let d = std::env::temp_dir().join(format!("syncdash-e2e-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            dirs.push(d.clone());
            Arc::new(LocalVfs::new(d)) as Arc<dyn Vfs>
        };
        run_all("local", &mut mk)
    };
    for d in dirs {
        let _ = std::fs::remove_dir_all(&d);
    }
    assert!(!rep.ran.is_empty(), "a lane that ran no cases is not a passing lane");
}

/// `MemVfs` wearing SFTP's declared shape: second-granularity timestamps and a rename that refuses
/// to overwrite.
#[test]
fn sftp_shaped_lane_syncs() {
    let mut n = 0;
    let rep = run_all("sftp-shaped", &mut || {
        n += 1;
        Arc::new(MemVfs::new(&format!("e2e-sftp-{n}")).without(|c| {
            c.mtime_precision_ms = 1_000;
            c.rename_overwrite = Support::No;
            c.fsync = Support::Unknown;
            c.free_space = Support::No;
            c.write_at = Support::No;
        })) as Arc<dyn Vfs>
    });
    assert!(!rep.ran.is_empty());
}

fn ftp_list_only() -> Arc<dyn Vfs> {
    Arc::new(MemVfs::new("e2e-ftp").without(|c| {
        c.mtime_precision_ms = 60_000;
        c.ranged_read = Support::No;
        c.set_mtime = Support::No;
        c.symlink = Support::No;
        c.unix_mode = Support::No;
        c.read_back = Support::No;
        c.free_space = Support::No;
        c.write_at = Support::No;
        c.rename_overwrite = Support::Unknown;
    })) as Arc<dyn Vfs>
}

/// `MemVfs` wearing the worst FTP shape — LIST-only, so no ranged reads, no `set_mtime`, and a
/// sixty-second view of time.
///
/// Its contract is that it **cannot be written at all**: the root lock's heartbeat is a repeated
/// `set_mtime`, so a remote backend without one has no way to tell another machine it is still
/// alive, and the write side refuses rather than sync without that. Every case therefore skips, and
/// the skip set being *complete* is the assertion — if some future change let one of them through,
/// this test fails, which is exactly what should happen.
#[test]
fn ftp_list_only_lane_is_readable_but_never_writable() {
    let rep = run_all("ftp-list-only", &mut ftp_list_only);
    assert!(
        rep.ran.is_empty(),
        "a root that cannot hold a lock must not have applied anything: {:?}",
        rep.ran
    );
    assert_eq!(rep.skipped.len(), cases::ALL.len(), "every case should have skipped, not just some");
    assert!(
        rep.skipped.iter().all(|(_, n)| *n == Need::WritableTarget),
        "the reason must be the lock, not an incidental capability: {:?}",
        rep.skipped
    );
}

/// The other half of that contract, and the half that matters to a user: comparing still works. A
/// LIST-only server is not useless — it is read-only, and the refusal says so in as many words
/// rather than failing somewhere in the middle of a write.
#[test]
fn ftp_list_only_still_compares_and_says_why_it_will_not_write() {
    let (sv, tv) = (ftp_list_only(), ftp_list_only());
    corpus::seed_into(&sv, corpus::BASE);
    corpus::seed_into(&tv, corpus::BASE);
    corpus::apply_edits(
        &sv,
        &[Edit::Add(Seed { path: "new.txt", seed: 5, size: 512, mtime_ms: 1_767_225_600_000 })],
    );

    let job = bare_job();
    let (said, ctx) = watched();

    let out = crate::run::local::compare_resolved(&job, &sv, &tv, &ctx, true)
        .expect("a read-only backend must still compare");
    assert_eq!(out.plan.ops.len(), 1, "the comparison itself is unaffected");

    // Both sides lose sampling together: a `~` digest can only ever match another `~` digest, so a
    // one-sided upgrade would make identical files look different.
    assert_eq!(
        out.source.header.vfs.as_ref().map(|v| v.evidence_effective.as_str()),
        Some("full"),
        "no ranged reads on either side means both sides read whole"
    );

    let ap = crate::run::local::apply_resolved(
        &job,
        &out.plan,
        &out.plan.ops,
        &sv,
        &tv,
        None,
        false,
        false,
        true,
        std::time::Instant::now(),
        &ctx,
    );
    assert_eq!(ap.errors, 1, "the write side must refuse");
    assert_eq!(ap.done, 0, "and must not have done anything first");
    let text = said.text();
    assert!(
        text.contains("root lock") && text.contains("refusing to write"),
        "the refusal has to name the lock as the reason:\n{text}"
    );
}

/// A live lane: open a scratch root per call under `base_url`, and hold the backend to both
/// contracts.
///
/// Everything it makes lives under `base_url` and is removed again before *and* after, so "fresh and
/// empty" holds even if an earlier run died mid-suite.
///
/// The pipeline suite runs first and the backend contract second, so a backend with a known contract
/// gap still reports whether real syncs work on it — the more useful half of the answer, and the
/// half lost if the contract check panics first.
fn live_lane(lane: &str, base_url: &str) {
    let creds = crate::fs::vfs::cred::default_provider();
    let open = |url: &str| -> Arc<dyn Vfs> {
        let v = crate::fs::vfs::open(url, &creds).unwrap_or_else(|e| panic!("opening '{url}': {e}"));
        v.connect().unwrap_or_else(|e| panic!("connecting to '{url}': {e}"));
        v
    };

    let base = open(base_url);
    let mut roots: Vec<String> = Vec::new();
    let mut n = 0usize;
    let mut mk = || {
        n += 1;
        let name = format!("e2e-{}-{n}", std::process::id());
        conformance::remove_tree(&base, &name).unwrap_or_else(|e| panic!("clearing '{name}': {e}"));
        base.mkdir_all(&name).unwrap_or_else(|e| panic!("creating '{name}': {e}"));
        roots.push(name.clone());
        open(&format!("{base_url}/{name}"))
    };

    let rep = run_all(lane, &mut mk);
    println!("[{lane}] ran {} case(s), skipped {:?}", rep.ran.len(), rep.skipped);
    conformance::run_all(&mut mk);

    for name in &roots {
        conformance::remove_tree(&base, name)
            .unwrap_or_else(|e| panic!("cleaning up '{name}': {e}"));
    }
    assert!(!rep.ran.is_empty(), "a live lane that ran no cases proves nothing");
}

/// ```text
/// set SYNCDASH_E2E_SFTP_URL=sftp://user@host/path/to/scratch
/// cargo test --lib sftp_live_lane -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a live SFTP server in SYNCDASH_E2E_SFTP_URL"]
fn sftp_live_lane() {
    let url = std::env::var("SYNCDASH_E2E_SFTP_URL")
        .expect("set SYNCDASH_E2E_SFTP_URL to an sftp://user@host/scratch phrase");
    live_lane("sftp", &url);
}

/// Needs a stored credential as well as a server — an `smb://` root cannot ride this machine's
/// session login the way a `\\host\share` path can.
///
/// ```text
/// syncdash cred set "smb://user@host/share"
/// set SYNCDASH_E2E_SMB_URL=smb://user@host/share/scratch
/// cargo test --lib smb_live_lane -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a live SMB share in SYNCDASH_E2E_SMB_URL and a stored credential"]
fn smb_live_lane() {
    let url = std::env::var("SYNCDASH_E2E_SMB_URL")
        .expect("set SYNCDASH_E2E_SMB_URL to an smb://user@host/share/scratch phrase");
    live_lane("smb", &url);
}

/// ```text
/// set SYNCDASH_E2E_FTP_URL=ftp://anonymous@host:2121/
/// cargo test --lib ftp_live_lane -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a live FTP server in SYNCDASH_E2E_FTP_URL"]
fn ftp_live_lane() {
    let url = std::env::var("SYNCDASH_E2E_FTP_URL")
        .expect("set SYNCDASH_E2E_FTP_URL to an ftp://user@host/scratch phrase");
    live_lane("ftp", &url);
}

/// The same backend over an authenticated TLS control channel.
///
/// The interesting half is what has to be true before this can run at all: the server's certificate
/// must verify against **this machine's own trust store**, because there is deliberately no flag to
/// skip verification. So a passing run is evidence for the design choice, not just for the code — a
/// LAN server whose certificate its owner installed is exactly the case `tls_connector` exists to
/// serve.
///
/// ```text
/// set SYNCDASH_E2E_FTPS_URL=ftps://anonymous@host:2122/
/// cargo test --lib ftps_live_lane -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a live FTPS server in SYNCDASH_E2E_FTPS_URL whose CA this machine trusts"]
fn ftps_live_lane() {
    let url = std::env::var("SYNCDASH_E2E_FTPS_URL")
        .expect("set SYNCDASH_E2E_FTPS_URL to an ftps://user@host/scratch phrase");
    live_lane("ftps", &url);
}

/// A real local disk other than the temp volume — an exFAT external drive, whose 10 ms mtime
/// granularity is a tier no other lane covers.
///
/// ```text
/// set SYNCDASH_E2E_EXFAT_ROOT=E:\syncdash-e2e
/// cargo test --lib exfat_live_lane -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a writable directory on a non-temp volume in SYNCDASH_E2E_EXFAT_ROOT"]
fn exfat_live_lane() {
    let root = std::env::var("SYNCDASH_E2E_EXFAT_ROOT")
        .expect("set SYNCDASH_E2E_EXFAT_ROOT to a writable directory on the volume under test");
    let base = std::path::PathBuf::from(&root);
    std::fs::create_dir_all(&base).expect("the scratch root must be creatable");

    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut n = 0usize;
    let rep = {
        let mut mk = || {
            n += 1;
            let d = base.join(format!("e2e-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            dirs.push(d.clone());
            Arc::new(LocalVfs::new(d)) as Arc<dyn Vfs>
        };
        conformance::run_all(&mut mk);
        run_all("exfat", &mut mk)
    };
    let caps = LocalVfs::new(base.clone()).caps();
    let precision = caps.mtime_precision_ms;
    assert_eq!(caps.unix_mode, Support::No, "exFAT cannot preserve Unix modes");
    #[cfg(target_os = "macos")]
    assert_eq!(caps.symlink, Support::Yes, "macOS FSKit exFAT supports symbolic links");
    assert_eq!(caps.file_id, Support::No, "exFAT object IDs are not durable rename evidence");

    let id_probe = base.join(format!("file-id-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&id_probe);
    std::fs::create_dir_all(&id_probe).unwrap();
    std::fs::write(id_probe.join("empty.bin"), b"").unwrap();
    let snapshot = crate::pipeline::scan::scan(
        &id_probe,
        &crate::pipeline::scan::ScanOptions {
            hash: false,
            sampled: false,
            use_cache: false,
            symlinks_direct: false,
            filter: crate::pipeline::filter::PathFilter::build(&[], &[]),
        },
    )
    .unwrap();
    assert!(
        snapshot.entries.iter().all(|entry| entry.file_id.is_none()),
        "exFAT snapshots must omit unstable synthetic object IDs",
    );
    let _ = std::fs::remove_dir_all(&id_probe);
    for d in dirs {
        let _ = std::fs::remove_dir_all(&d);
    }
    println!("[exfat] {root} reports {precision} ms mtime precision; skipped {:?}", rep.skipped);
    assert!(!rep.ran.is_empty());
}
