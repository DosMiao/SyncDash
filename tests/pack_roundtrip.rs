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
    let d = std::env::temp_dir().join(format!("syncdash-packtest-{}-{name}", std::process::id()));
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
            schema: syncdash::model::plan::PLAN_SCHEMA,
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

    let plan = plan_of(
        &tgt.to_string_lossy(),
        vec![copy_op("a/one.txt"), copy_op("two.bin")],
    );
    let summary = pack::pack(&plan, &src, &out, None).unwrap();
    assert_eq!(summary.files, 2, "both copy ops must ride in the payload");
    assert!(out.exists());

    let (done, _skipped, errors) = pack::apply_pack(&out, Some(&tgt), true, false, false).unwrap();
    assert_eq!(errors, 0, "a clean package must apply without error");
    assert_eq!(done, 2);
    assert_eq!(
        std::fs::read(tgt.join("a/one.txt")).unwrap(),
        b"hello package"
    );
    assert_eq!(std::fs::read(tgt.join("two.bin")).unwrap(), vec![7u8; 5000]);
}

#[cfg(unix)]
#[test]
fn package_metadata_is_applied_by_the_leased_write_transaction() {
    use std::os::unix::fs::PermissionsExt;

    let src = tmp("src-mode");
    let tgt = tmp("tgt-mode");
    let out = tmp("pkg-mode").join("p.tar");
    write(&src, "script.sh", b"#!/bin/sh\nexit 0\n");
    std::fs::set_permissions(
        src.join("script.sh"),
        std::fs::Permissions::from_mode(0o751),
    )
    .unwrap();

    let plan = plan_of(&tgt.to_string_lossy(), vec![copy_op("script.sh")]);
    pack::pack(&plan, &src, &out, None).unwrap();
    let (done, _, errors) = pack::apply_pack(&out, Some(&tgt), true, false, false).unwrap();

    assert_eq!((done, errors), (1, 0));
    assert_eq!(
        std::fs::metadata(tgt.join("script.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o751
    );
}

fn update_op(rel: &str) -> Op {
    Op {
        action: Action::Update,
        ..copy_op(rel)
    }
}

fn delete_op(rel: &str) -> Op {
    Op {
        action: Action::Delete,
        ..copy_op(rel)
    }
}

fn body(seed: u32, len: usize) -> Vec<u8> {
    (0..len as u32)
        .map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(seed) >> 13) as u8)
        .collect()
}

fn compare_plan(src: &Path, tgt: &Path, rigor: &str) -> Plan {
    let job = syncdash::job::Job {
        source: src.to_string_lossy().into_owned(),
        targets: vec![tgt.to_string_lossy().into_owned()],
        rigor: rigor.into(),
        exclude: Vec::new(),
        ..Default::default()
    };
    let selected = job.select_target(0).unwrap();
    syncdash::run::local::compare_job_detailed(&selected, &syncdash::obs::progress::RunCtx::null())
        .unwrap()
        .plan
}

/// Pack `rel` as a delta against the copy the target already holds.
fn pack_delta(src: &Path, tgt: &Path, out: &Path, rel: &str) -> pack::PackSummary {
    let rel_path = syncdash::foundation::path::RootRelativePath::try_from(rel).unwrap();
    let target_root = syncdash::fs::local_root::LocalRoot::open(tgt.to_path_buf()).unwrap();
    let base = syncdash::fs::chunk::chunk_file(&target_root, &rel_path).unwrap();
    let plan = plan_of(&tgt.to_string_lossy(), vec![update_op(rel)]);
    pack::pack(
        &plan,
        src,
        out,
        Some(&[(rel.to_string(), base)].into_iter().collect()),
    )
    .unwrap()
}

/// The delta lane end to end — the half of this module the roundtrip above never reached.
///
/// `apply_pack` hashes the base off the target root and then re-reads it through the recipe. Both
/// used to be separate opens with the hash coming from `update_mmap_rayon`; they are one handle and
/// a chunked read now. The payload is deliberately larger than the 8 MiB read granularity so the
/// hash loop runs more than once.
#[test]
fn packs_and_applies_a_delta() {
    let src = tmp("src-delta");
    let tgt = tmp("tgt-delta");
    let out = tmp("pkg-delta").join("p.tar");

    let old = body(0, 10 * 1024 * 1024);
    let mut new = old.clone();
    new[6_000_000..6_001_000].fill(0xAB);
    write(&tgt, "big.bin", &old);
    write(&src, "big.bin", &new);

    let summary = pack_delta(&src, &tgt, &out, "big.bin");
    assert!(
        summary.delta_saved > 0,
        "an edited stretch must ride as a delta, not as the whole file"
    );

    let (done, _skipped, errors) = pack::apply_pack(&out, Some(&tgt), true, false, false).unwrap();
    assert_eq!(
        (done, errors),
        (1, 0),
        "a matching base must reassemble without error"
    );
    assert_eq!(
        std::fs::read(tgt.join("big.bin")).unwrap(),
        new,
        "reassembly must be byte-exact"
    );
}

#[test]
fn whole_pack_refuses_content_that_changed_after_compare() {
    let src = tmp("src-stale-whole-evidence");
    let tgt = tmp("tgt-stale-whole-evidence");
    let out = tmp("pkg-stale-whole-evidence").join("p.tar");
    let original = body(17, 256 * 1024);
    write(&src, "big.bin", &original);
    let plan = compare_plan(&src, &tgt, "paranoid");
    assert!(plan.ops[0]
        .hash
        .as_deref()
        .is_some_and(|hash| !hash.starts_with('~')));

    let midpoint = original.len() / 2;
    let mut changed = original;
    changed[midpoint] ^= 0xFF;
    write(&src, "big.bin", &changed);

    let error = match pack::pack(&plan, &src, &out, None) {
        Ok(_) => panic!("changed source evidence must not be packaged"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("content changed after Compare"));
}

#[test]
fn metadata_only_pack_refuses_timestamp_drift_after_compare() {
    let src = tmp("src-stale-metadata-evidence");
    let tgt = tmp("tgt-stale-metadata-evidence");
    let out = tmp("pkg-stale-metadata-evidence").join("p.tar");
    write(&src, "same-size.bin", b"before");
    let plan = compare_plan(&src, &tgt, "quick");
    assert!(plan.ops[0].hash.is_none());

    write(&src, "same-size.bin", b"after!");
    filetime::set_file_mtime(
        src.join("same-size.bin"),
        filetime::FileTime::from_unix_time(2_000_000_000, 0),
    )
    .unwrap();

    let error = match pack::pack(&plan, &src, &out, None) {
        Ok(_) => panic!("changed metadata evidence must not be packaged"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("timestamp changed after Compare"));
}

#[test]
fn delta_pack_refuses_sampled_content_that_changed_after_compare() {
    let src = tmp("src-stale-delta-evidence");
    let tgt = tmp("tgt-stale-delta-evidence");
    let out = tmp("pkg-stale-delta-evidence").join("p.tar");
    let old = body(
        21,
        syncdash::model::chunk::DELTA_MIN_SIZE as usize + 1024 * 1024,
    );
    let mut current = old.clone();
    let sampled_middle = current.len() / 2;
    current[sampled_middle..sampled_middle + 1_000].fill(0x5A);
    write(&src, "big.bin", &current);
    write(&tgt, "big.bin", &old);
    let plan = compare_plan(&src, &tgt, "standard");
    assert!(plan.ops[0]
        .hash
        .as_deref()
        .is_some_and(|hash| hash.starts_with('~')));
    let target_root = syncdash::fs::local_root::LocalRoot::open(tgt.clone()).unwrap();
    let relative = syncdash::foundation::path::RootRelativePath::try_from("big.bin").unwrap();
    let base = syncdash::fs::chunk::chunk_file(&target_root, &relative).unwrap();

    current[10] ^= 0xFF;
    write(&src, "big.bin", &current);
    let peer_chunks = [("big.bin".to_string(), base)].into_iter().collect();

    let error = match pack::pack(&plan, &src, &out, Some(&peer_chunks)) {
        Ok(_) => panic!("changed sampled evidence must not be packaged"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("content changed after Compare"));
}

/// A recipe is only meaningful against the exact bytes it was computed from. If the target moved on
/// after the chunk table was taken, patching it would splice two different files together — so the
/// base hash has to catch it, and the whole package aborts rather than landing part of itself.
#[test]
fn a_delta_base_that_changed_is_refused() {
    let src = tmp("src-stale");
    let tgt = tmp("tgt-stale");
    let out = tmp("pkg-stale").join("p.tar");

    let old = body(
        1,
        syncdash::model::chunk::DELTA_MIN_SIZE as usize + 1024 * 1024,
    );
    let mut new = old.clone();
    new[2_500_000..2_501_000].fill(0xCD);
    write(&tgt, "big.bin", &old);
    write(&src, "big.bin", &new);
    pack_delta(&src, &tgt, &out, "big.bin");

    let mut drifted = old.clone();
    drifted[10..20].fill(0xEE);
    write(&tgt, "big.bin", &drifted);

    assert!(
        pack::apply_pack(&out, Some(&tgt), true, false, false).is_err(),
        "a stale base must abort the package"
    );
    assert_eq!(
        std::fs::read(tgt.join("big.bin")).unwrap(),
        drifted,
        "and nothing may have been written"
    );
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
    assert!(
        !tgt.join("f.txt").exists(),
        "do_apply=false must not touch the target"
    );
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
    assert!(
        e.to_string().contains("newer than this binary supports"),
        "{e}"
    );
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
        let s = String::from_utf8(orig.to_vec())
            .unwrap()
            .replace("f.txt", "../escaped.txt");
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

#[test]
fn a_missing_payload_aborts_before_any_plan_operation() {
    let src = tmp("src-missing-payload");
    let tgt = tmp("tgt-missing-payload");
    let out = tmp("pkg-missing-payload").join("p.tar");
    write(&src, "new.txt", b"new");
    write(&tgt, "keep.txt", b"must survive");
    let plan = plan_of(
        &tgt.to_string_lossy(),
        vec![copy_op("new.txt"), delete_op("keep.txt")],
    );
    pack::pack(&plan, &src, &out, None).unwrap();
    let stripped = tmp("pkg-missing-payload-2").join("p.tar");
    drop_member(&out, &stripped, "payload/new.txt");

    let error = pack::apply_pack(&stripped, Some(&tgt), true, false, false).unwrap_err();
    assert!(
        error.to_string().contains("missing from the tar archive"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(tgt.join("keep.txt")).unwrap(),
        b"must survive"
    );
    assert!(!tgt.join("new.txt").exists());
}

#[test]
fn duplicate_manifest_payloads_are_refused() {
    let src = tmp("src-duplicate-manifest");
    let tgt = tmp("tgt-duplicate-manifest");
    let out = tmp("pkg-duplicate-manifest").join("p.tar");
    write(&src, "f.txt", b"payload");
    pack::pack(
        &plan_of(&tgt.to_string_lossy(), vec![copy_op("f.txt")]),
        &src,
        &out,
        None,
    )
    .unwrap();
    let repacked = tmp("pkg-duplicate-manifest-2").join("p.tar");
    rewrite_member(&out, &repacked, "manifest.json", |body| {
        let mut manifest: serde_json::Value = serde_json::from_slice(body).unwrap();
        let duplicate = manifest["payload"][0].clone();
        manifest["payload"].as_array_mut().unwrap().push(duplicate);
        serde_json::to_vec(&manifest).unwrap()
    });

    let error = pack::apply_pack(&repacked, Some(&tgt), true, false, false).unwrap_err();
    assert!(error.to_string().contains("duplicate payload"), "{error}");
}

#[test]
fn duplicate_tar_payloads_are_refused() {
    let src = tmp("src-duplicate-tar");
    let tgt = tmp("tgt-duplicate-tar");
    let out = tmp("pkg-duplicate-tar").join("p.tar");
    write(&src, "f.txt", b"payload");
    pack::pack(
        &plan_of(&tgt.to_string_lossy(), vec![copy_op("f.txt")]),
        &src,
        &out,
        None,
    )
    .unwrap();
    let repacked = tmp("pkg-duplicate-tar-2").join("p.tar");
    append_member(&out, &repacked, "payload/f.txt", b"payload");

    let error = pack::apply_pack(&repacked, Some(&tgt), true, false, false).unwrap_err();
    assert!(error.to_string().contains("duplicate payload"), "{error}");
}

#[test]
fn a_tampered_combined_payload_digest_is_refused() {
    let src = tmp("src-combined-digest");
    let tgt = tmp("tgt-combined-digest");
    let out = tmp("pkg-combined-digest").join("p.tar");
    write(&src, "f.txt", b"payload");
    pack::pack(
        &plan_of(&tgt.to_string_lossy(), vec![copy_op("f.txt")]),
        &src,
        &out,
        None,
    )
    .unwrap();
    let repacked = tmp("pkg-combined-digest-2").join("p.tar");
    rewrite_member(&out, &repacked, "manifest.json", |body| {
        let mut manifest: serde_json::Value = serde_json::from_slice(body).unwrap();
        manifest["payload_combined_blake3"] = serde_json::json!("0".repeat(64));
        serde_json::to_vec(&manifest).unwrap()
    });

    let error = pack::apply_pack(&repacked, Some(&tgt), true, false, false).unwrap_err();
    assert!(
        error.to_string().contains("combined digest mismatch"),
        "{error}"
    );
}

#[test]
fn an_older_pack_version_is_refused() {
    let src = tmp("src-old-version");
    let tgt = tmp("tgt-old-version");
    let out = tmp("pkg-old-version").join("p.tar");
    write(&src, "f.txt", b"payload");
    pack::pack(
        &plan_of(&tgt.to_string_lossy(), vec![copy_op("f.txt")]),
        &src,
        &out,
        None,
    )
    .unwrap();
    let repacked = tmp("pkg-old-version-2").join("p.tar");
    rewrite_member(&out, &repacked, "manifest.json", |body| {
        let mut manifest: serde_json::Value = serde_json::from_slice(body).unwrap();
        manifest["pack_version"] = serde_json::json!(pack::PACK_VERSION - 1);
        serde_json::to_vec(&manifest).unwrap()
    });

    let error = pack::apply_pack(&repacked, Some(&tgt), true, false, false).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("older than this binary requires"),
        "{error}"
    );
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

fn append_member(src: &Path, dst: &Path, name: &str, body: &[u8]) {
    let mut members = read_members(src);
    members.push((name.to_owned(), body.to_vec()));
    let out = std::fs::File::create(dst).unwrap();
    let mut builder = tar::Builder::new(out);
    for (member_name, member_body) in members {
        let mut header = tar::Header::new_gnu();
        header.set_size(member_body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, &member_name, &member_body[..])
            .unwrap();
    }
    builder.finish().unwrap();
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
