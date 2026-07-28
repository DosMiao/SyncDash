//! The same filename spelled two ways.
//!
//! Windows writes `é` as one code point (NFC); macOS hands back the decomposed form, `e` followed by
//! a combining acute (NFD). Same name to a person, different byte strings to a comparison — so a
//! tool matching on raw bytes sees one file missing and one file extra, and in `mirror` deletes the
//! target's copy and re-sends the source's, on every single run, forever.
//!
//! The two spellings below are deliberately non-ASCII and must survive any tidying sweep: an ASCII
//! pair would assert nothing at all, the same reason the CJK fixtures in `foundation::text` are kept.

use std::sync::Arc;

use super::*;
use crate::fs::vfs::local::LocalVfs;
use crate::fs::vfs::memory::MemVfs;
use crate::fs::vfs::{Vfs, WriteHint};

const NFC_NAME: &str = "unicode/caf\u{00e9}.txt"; // é as U+00E9
const NFD_NAME: &str = "unicode/cafe\u{0301}.txt"; // e + U+0301 combining acute

const PAYLOAD_SEED: u64 = 77;
const PAYLOAD_SIZE: usize = 2_048;
const STAMP: i64 = 1_767_225_600_000;

/// Place the identical content under a given spelling. Written through the `Vfs` write API so the
/// name reaches the backend exactly as given, rather than through whatever the host shell would do
/// to it.
fn place(root: &Arc<dyn Vfs>, name: &str) {
    let payload = crate::fs::vfs::memory::filler(PAYLOAD_SEED, PAYLOAD_SIZE);
    root.mkdir_all("unicode").unwrap();
    let hint =
        WriteHint { size_hint: Some(payload.len() as u64), mtime_ms: Some(STAMP), mode: None };
    let mut w = root.open_write(name, &hint).unwrap();
    w.write(&payload).unwrap();
    w.seal(false).unwrap();
    w.commit().unwrap();
    let _ = root.set_mtime(name, STAMP);
}

#[test]
fn the_same_name_in_nfc_and_nfd_is_one_file() {
    let sv: Arc<dyn Vfs> = Arc::new(MemVfs::new("uni-src"));
    let tv: Arc<dyn Vfs> = Arc::new(MemVfs::new("uni-tgt"));
    place(&sv, NFC_NAME);
    place(&tv, NFD_NAME);

    let (_, ctx) = watched();
    let out =
        crate::run::local::compare_resolved(&bare_job(), &sv, &tv, &ctx, true).expect("compare");
    assert!(
        out.plan.ops.is_empty(),
        "the same name spelled two ways is one file — any op here is a re-transfer that would \
         repeat on every run: {:?}",
        out.plan.ops
    );
}

/// The same question against a real macOS filesystem, where the decomposition is not a fixture but
/// the platform's own doing. Runs on the SFTP lane because that is the pair actually synced: NTFS on
/// one end, APFS on the other.
#[test]
#[ignore = "needs a live SFTP server in SYNCDASH_E2E_SFTP_URL"]
fn nfc_and_nfd_agree_across_a_real_windows_mac_pair() {
    let base_url = std::env::var("SYNCDASH_E2E_SFTP_URL")
        .expect("set SYNCDASH_E2E_SFTP_URL to an sftp://user@host/scratch phrase");
    let creds = crate::fs::vfs::cred::default_provider();
    let open = |url: &str| -> Arc<dyn Vfs> {
        let v = crate::fs::vfs::open(url, &creds).unwrap_or_else(|e| panic!("open {url}: {e}"));
        v.connect().unwrap_or_else(|e| panic!("connect {url}: {e}"));
        v
    };
    let base = open(&base_url);
    let name = format!("uni-{}", std::process::id());
    crate::fs::vfs::conformance::remove_tree(&base, &name).unwrap();
    base.mkdir_all(&name).unwrap();
    let tv = open(&format!("{base_url}/{name}"));

    let local = std::env::temp_dir().join(format!("syncdash-uni-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local);
    std::fs::create_dir_all(&local).unwrap();
    let sv: Arc<dyn Vfs> = Arc::new(LocalVfs::new(local.clone()));

    place(&sv, NFC_NAME);
    place(&tv, NFD_NAME);

    let (_, ctx) = watched();
    let out =
        crate::run::local::compare_resolved(&bare_job(), &sv, &tv, &ctx, true).expect("compare");
    let ops = out.plan.ops.clone();

    crate::fs::vfs::conformance::remove_tree(&base, &name).unwrap();
    let _ = std::fs::remove_dir_all(&local);

    assert!(ops.is_empty(), "NTFS wrote NFC, APFS holds NFD, and they are the same file — {ops:?}");
}
