//! `transfer::pack` end to end, and every gate `apply_pack` puts in front of a package.
//!
//! These matter more than their line count suggests: `apply_pack` runs on the **far** machine,
//! fed a tar that arrived over an ssh pipe. Its version check, plan-hash check, path-safety
//! check and per-file hash check are the whole trust boundary, and until this file existed the
//! entire module had zero tests.

use std::path::{Path, PathBuf};

use syncdash::model::plan::{Action, Op, Plan, PlanHeader, Side};
use syncdash::transfer::pack;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir()
        .join(format!("syncdash-packtest-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(root: &Path, rel: &str, body: &[u8]) {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, body).unwrap();
}

fn plan_of(target_root: &str, ops: Vec<Op>) -> Plan {
    Plan {
        header: PlanHeader {
            schema: 1,
            kind: "plan".into(),
            mode: "mirror".into(),
            generated_at_ms: 0,
            source_root: "SRC".into(),
            source_host: "test".into(),
            target_root: target_root.into(),
            target_host: "test".into(),
            op_count: ops.len() as u64,
            conflict_count: 0,
            source_entries: 0,
            target_entries: 0,
            source_excluded: 0,
            target_excluded: 0,
            source_walk_errors: 0,
            target_walk_errors: 0,
            source_walk_err_samples: Vec::new(),
            target_walk_err_samples: Vec::new(),
            source_icloud_stubs: 0,
            target_icloud_stubs: 0,
            source_icloud_stub_samples: Vec::new(),
            target_icloud_stub_samples: Vec::new(),
        },
        ops,
    }
}

fn copy_op(rel: &str) -> Op {
    Op {
        side: Side::Target,
        action: Action::Copy,
        path: rel.into(),
        from: None,
        size: None,
        mtime_ms: None,
        hash: None,
        link: None,
        mode: None,
        reason: "test".into(),
    }
}

/// pack -> apply_pack against a real directory: the payload lands, byte for byte.
#[test]
fn packs_and_applies_a_copy() {
    let src = tmp("src-ok");
    let tgt = tmp("tgt-ok");
    let out = tmp("pkg-ok").join("p.tar");
    write(&src, "a/one.txt", b"hello package");
    write(&src, "two.bin", &[7u8; 5000]);

    let plan = plan_of(&tgt.to_string_lossy(), vec![copy_op("a/one.txt"), copy_op("two.bin")]);
    let summary = pack::pack(&plan, &src, &out, None).unwrap();
    assert_eq!(summary.files, 2, "both copy ops must ride in the payload");
    assert!(out.exists());

    let (done, _skipped, errors) = pack::apply_pack(&out, Some(&tgt), true, false, false).unwrap();
    assert_eq!(errors, 0, "a clean package must apply without error");
    assert_eq!(done, 2);
    assert_eq!(std::fs::read(tgt.join("a/one.txt")).unwrap(), b"hello package");
    assert_eq!(std::fs::read(tgt.join("two.bin")).unwrap(), vec![7u8; 5000]);
}

/// Dry run is the default posture everywhere else in this tool; a package is no exception.
#[test]
fn dry_run_writes_nothing() {
    let src = tmp("src-dry");
    let tgt = tmp("tgt-dry");
    let out = tmp("pkg-dry").join("p.tar");
    write(&src, "f.txt", b"payload");

    let plan = plan_of(&tgt.to_string_lossy(), vec![copy_op("f.txt")]);
    pack::pack(&plan, &src, &out, None).unwrap();
    pack::apply_pack(&out, Some(&tgt), false, false, false).unwrap();
    assert!(!tgt.join("f.txt").exists(), "do_apply=false must not touch the target");
}

/// The manifest's plan hash is the package's identity. Editing the plan after packing —
/// the obvious attack on a tar that crossed a network — must be refused before anything runs.
#[test]
fn a_tampered_plan_is_refused() {
    let src = tmp("src-tamper");
    let tgt = tmp("tgt-tamper");
    let out = tmp("pkg-tamper").join("p.tar");
    write(&src, "f.txt", b"payload");
    let plan = plan_of(&tgt.to_string_lossy(), vec![copy_op("f.txt")]);
    pack::pack(&plan, &src, &out, None).unwrap();

    let repacked = tmp("pkg-tamper2").join("p.tar");
    rewrite_member(&out, &repacked, "plan.jsonl", |orig| {
        let mut s = String::from_utf8(orig.to_vec()).unwrap();
        s = s.replace("f.txt", "g.txt");
        s.into_bytes()
    });

    let e = pack::apply_pack(&repacked, Some(&tgt), true, false, false).unwrap_err();
    assert!(e.to_string().contains("plan hash mismatch"), "{e}");
    assert!(!tgt.join("g.txt").exists());
}

/// A package built by a newer binary must be refused with an actionable message rather than
/// half-applied by a format this build only partly understands.
#[test]
fn a_future_pack_version_is_refused() {
    let src = tmp("src-ver");
    let tgt = tmp("tgt-ver");
    let out = tmp("pkg-ver").join("p.tar");
    write(&src, "f.txt", b"payload");
    let plan = plan_of(&tgt.to_string_lossy(), vec![copy_op("f.txt")]);
    pack::pack(&plan, &src, &out, None).unwrap();

    let repacked = tmp("pkg-ver2").join("p.tar");
    rewrite_member(&out, &repacked, "manifest.json", |orig| {
        let mut m: serde_json::Value = serde_json::from_slice(orig).unwrap();
        m["pack_version"] = serde_json::json!(pack::PACK_VERSION + 9);
        serde_json::to_vec(&m).unwrap()
    });

    let e = pack::apply_pack(&repacked, Some(&tgt), true, false, false).unwrap_err();
    assert!(e.to_string().contains("newer than this binary supports"), "{e}");
}

/// Path safety is checked on the plan itself, so a traversal cannot even be attempted.
/// `..` must never resolve outside the target root.
#[test]
fn an_escaping_path_is_refused() {
    let src = tmp("src-esc");
    let tgt = tmp("tgt-esc");
    let out = tmp("pkg-esc").join("p.tar");
    write(&src, "f.txt", b"payload");
    let plan = plan_of(&tgt.to_string_lossy(), vec![copy_op("f.txt")]);
    pack::pack(&plan, &src, &out, None).unwrap();

    let repacked = tmp("pkg-esc2").join("p.tar");
    // Rewrite the plan AND the manifest hash, so the escape is what gets caught rather than
    // the hash — otherwise this test would pass for the wrong reason.
    let mut new_plan = Vec::new();
    rewrite_member(&out, &repacked, "plan.jsonl", |orig| {
        let s = String::from_utf8(orig.to_vec()).unwrap().replace("f.txt", "../escaped.txt");
        new_plan = s.clone().into_bytes();
        new_plan.clone()
    });
    let fixed = tmp("pkg-esc3").join("p.tar");
    rewrite_member(&repacked, &fixed, "manifest.json", |orig| {
        let mut m: serde_json::Value = serde_json::from_slice(orig).unwrap();
        m["plan_blake3"] = serde_json::json!(blake3::hash(&new_plan).to_hex().to_string());
        serde_json::to_vec(&m).unwrap()
    });

    let e = pack::apply_pack(&fixed, Some(&tgt), true, false, false).unwrap_err();
    assert!(e.to_string().contains("unsafe path"), "{e}");
    assert!(!tgt.parent().unwrap().join("escaped.txt").exists());
}

/// A package missing its manifest is not a package.
#[test]
fn a_package_without_a_manifest_is_refused() {
    let src = tmp("src-nom");
    let tgt = tmp("tgt-nom");
    let out = tmp("pkg-nom").join("p.tar");
    write(&src, "f.txt", b"payload");
    let plan = plan_of(&tgt.to_string_lossy(), vec![copy_op("f.txt")]);
    pack::pack(&plan, &src, &out, None).unwrap();

    let stripped = tmp("pkg-nom2").join("p.tar");
    drop_member(&out, &stripped, "manifest.json");

    let e = pack::apply_pack(&stripped, Some(&tgt), true, false, false).unwrap_err();
    assert!(e.to_string().contains("no manifest.json"), "{e}");
}

// ---- tar surgery helpers: rebuild a package with one member replaced or dropped ----

fn rewrite_member(src: &Path, dst: &Path, member: &str, mut f: impl FnMut(&[u8]) -> Vec<u8>) {
    let members = read_members(src);
    let out = std::fs::File::create(dst).unwrap();
    let mut b = tar::Builder::new(out);
    for (name, body) in members {
        let body = if name == member { f(&body) } else { body };
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, &name, &body[..]).unwrap();
    }
    b.finish().unwrap();
}

fn drop_member(src: &Path, dst: &Path, member: &str) {
    let members = read_members(src);
    let out = std::fs::File::create(dst).unwrap();
    let mut b = tar::Builder::new(out);
    for (name, body) in members {
        if name == member {
            continue;
        }
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, &name, &body[..]).unwrap();
    }
    b.finish().unwrap();
}

fn read_members(src: &Path) -> Vec<(String, Vec<u8>)> {
    use std::io::Read;
    let f = std::fs::File::open(src).unwrap();
    let mut ar = tar::Archive::new(std::io::BufReader::new(f));
    let mut out = Vec::new();
    for e in ar.entries().unwrap() {
        let mut e = e.unwrap();
        let name = e.path().unwrap().to_string_lossy().into_owned();
        let mut body = Vec::new();
        e.read_to_end(&mut body).unwrap();
        out.push((name, body));
    }
    out
}
