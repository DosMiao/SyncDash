//! Package application after protocol validation, with verified staging before target mutation.

mod staging;

use std::path::{Path, PathBuf};

use crate::model::plan::Action;
use crate::transfer::pack::format::codec::decode_and_validate;

use staging::stage_payloads;

/// Execute a package on the far end. `do_apply = false` only verifies and lists operations.
/// Returns `(done, skipped, errors)`.
pub fn apply_pack(
    package_path: &Path,
    target_root_override: Option<&Path>,
    do_apply: bool,
    verbose: bool,
    versioning: bool,
) -> std::io::Result<(u64, u64, u64)> {
    let mut package_file = std::fs::File::open(package_path)?;
    let decoded = decode_and_validate(&mut package_file)?;
    let plan = decoded.plan;
    let manifest = decoded.manifest;

    let target_root: PathBuf = match target_root_override {
        Some(path) => path.to_path_buf(),
        None => PathBuf::from(&plan.header.target_root),
    };
    println!(
        "package: {} op(s), {} payload file(s), from {} ({}) -> target {}",
        plan.ops.len(),
        manifest.payload.len(),
        manifest.source_host,
        manifest.source_os,
        target_root.display()
    );

    if !do_apply {
        for operation in &plan.ops {
            println!(
                "DRY  {:?} {}  ({})",
                operation.action, operation.path, operation.reason
            );
        }
        println!("dry-run OK: package structure and metadata verified; rerun with --apply");
        return Ok((0, plan.ops.len() as u64, 0));
    }

    if !target_root.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("target root not accessible: {}", target_root.display()),
        ));
    }

    let staging = stage_payloads(&mut package_file, &manifest, &target_root, verbose)?;
    let by_relative: std::collections::HashMap<&str, _> = manifest
        .payload
        .iter()
        .map(|payload| (payload.rel.as_str(), payload))
        .collect();

    // Materialize verified manifest metadata into each operation so content, timestamps, and modes
    // publish while the normal apply transaction still holds the root lease.
    let mut operations = plan.ops.clone();
    for operation in &mut operations {
        if matches!(operation.action, Action::Copy | Action::Update) {
            if let Some(payload) = by_relative.get(operation.path.as_str()) {
                operation.size = Some(payload.size);
                operation.hash = Some(payload.hash.clone());
                operation.mtime_ms = Some(payload.mtime_ms);
                operation.mode = payload.mode;
            }
        }
    }
    let (done, skipped, errors) = crate::pipeline::apply::apply(
        &operations,
        staging.path(),
        &target_root,
        &crate::pipeline::apply::ApplyOptions {
            dry_run: false,
            verbose,
            verify: true,
            versioning,
            ..Default::default()
        },
    );

    Ok((done, skipped, errors))
}
