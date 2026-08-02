//! Cross-checks among the decoded plan, manifest, and tar member table.

use crate::foundation::path::RootRelativePath;
use crate::model::plan::{Action, Plan, Side};
use crate::transfer::pack::format::{package_error, Manifest, PACK_SCHEMA};

fn validate_digest(value: &str, label: &str) -> std::io::Result<()> {
    if crate::model::digest::is_blake3_hex(value) {
        Ok(())
    } else {
        Err(package_error(format!(
            "{label} must be a lowercase 64-character BLAKE3 digest"
        )))
    }
}

pub(super) fn validate_package_structure(
    plan: &Plan,
    manifest: &Manifest,
    tar_payload: &std::collections::HashMap<RootRelativePath, u64>,
) -> std::io::Result<()> {
    if manifest.schema != PACK_SCHEMA {
        return Err(package_error(format!(
            "package table schema {} does not match this build ({})",
            manifest.schema, PACK_SCHEMA
        )));
    }
    if plan.header.kind != "plan" {
        return Err(package_error(format!(
            "package plan has unexpected kind {:?}",
            plan.header.kind
        )));
    }
    if plan.header.op_count != plan.ops.len() as u64 || manifest.op_count != plan.ops.len() as u64 {
        return Err(package_error(format!(
            "package operation counts disagree: plan header={}, manifest={}, decoded={}",
            plan.header.op_count,
            manifest.op_count,
            plan.ops.len()
        )));
    }
    if manifest.target_root_hint != plan.header.target_root {
        return Err(package_error(
            "manifest target root hint does not match the signed package plan",
        ));
    }

    let mut required_payload = std::collections::HashSet::new();
    for operation in &plan.ops {
        if operation.side != Side::Target || !operation.action.is_executable() {
            return Err(package_error(format!(
                "package plan contains an operation that cannot execute on the target: {:?} {:?}",
                operation.side, operation.action
            )));
        }
        let path = RootRelativePath::try_from(operation.path.as_str())
            .map_err(|error| package_error(format!("unsafe path in package plan: {error}")))?;
        if let Some(from) = &operation.from {
            RootRelativePath::try_from(from.as_str()).map_err(|error| {
                package_error(format!("unsafe source path in package plan: {error}"))
            })?;
        }
        if matches!(operation.action, Action::Copy | Action::Update)
            && operation.link.is_none()
            && !required_payload.insert(path.clone())
        {
            return Err(package_error(format!(
                "package plan requests the payload {:?} more than once",
                path.as_str()
            )));
        }
    }

    validate_digest(&manifest.plan_blake3, "manifest plan_blake3")?;
    validate_digest(
        &manifest.payload_combined_blake3,
        "manifest payload_combined_blake3",
    )?;

    let mut manifest_payload = std::collections::HashSet::new();
    let mut combined = blake3::Hasher::new();
    for payload in &manifest.payload {
        if !manifest_payload.insert(payload.rel.clone()) {
            return Err(package_error(format!(
                "manifest contains duplicate payload {:?}",
                payload.rel.as_str()
            )));
        }
        validate_digest(
            &payload.hash,
            &format!("payload {:?} hash", payload.rel.as_str()),
        )?;
        combined.update(payload.hash.as_bytes());
        let tar_size = tar_payload.get(&payload.rel).ok_or_else(|| {
            package_error(format!(
                "manifest payload {:?} is missing from the tar archive",
                payload.rel.as_str()
            ))
        })?;
        match payload.kind.as_str() {
            "whole"
                if payload.base_hash.is_none()
                    && payload.blob_hash.is_none()
                    && payload.recipe.is_none() =>
            {
                if *tar_size != payload.size {
                    return Err(package_error(format!(
                        "whole payload {:?} has tar size {} but manifest size {}",
                        payload.rel.as_str(),
                        tar_size,
                        payload.size
                    )));
                }
            }
            "delta"
                if payload.base_hash.is_some()
                    && payload.blob_hash.is_some()
                    && payload.recipe.is_some() =>
            {
                validate_digest(
                    payload.base_hash.as_deref().unwrap(),
                    &format!("payload {:?} base_hash", payload.rel.as_str()),
                )?;
                validate_digest(
                    payload.blob_hash.as_deref().unwrap(),
                    &format!("payload {:?} blob_hash", payload.rel.as_str()),
                )?;
                let mut reconstructed_size = 0u64;
                for step in payload.recipe.as_ref().unwrap() {
                    reconstructed_size = reconstructed_size
                        .checked_add(step.len as u64)
                        .ok_or_else(|| package_error("delta recipe size overflow"))?;
                    let end = step
                        .off
                        .checked_add(step.len as u64)
                        .ok_or_else(|| package_error("delta recipe range overflow"))?;
                    match step.s.as_str() {
                        "base" => {}
                        "blob" if end <= *tar_size => {}
                        "blob" => {
                            return Err(package_error(format!(
                                "delta recipe for {:?} reads beyond its blob",
                                payload.rel.as_str()
                            )))
                        }
                        other => {
                            return Err(package_error(format!(
                                "delta recipe for {:?} has unknown source {other:?}",
                                payload.rel.as_str()
                            )))
                        }
                    }
                }
                if reconstructed_size != payload.size {
                    return Err(package_error(format!(
                        "delta recipe for {:?} reconstructs {} bytes, expected {}",
                        payload.rel.as_str(),
                        reconstructed_size,
                        payload.size
                    )));
                }
            }
            "whole" => {
                return Err(package_error(format!(
                    "whole payload {:?} carries delta-only fields",
                    payload.rel.as_str()
                )))
            }
            "delta" => {
                return Err(package_error(format!(
                    "delta payload {:?} is structurally incomplete",
                    payload.rel.as_str()
                )))
            }
            other => {
                return Err(package_error(format!(
                    "payload {:?} has unknown kind {other:?}",
                    payload.rel.as_str()
                )))
            }
        }
    }

    let combined = combined.finalize().to_hex().to_string();
    if combined != manifest.payload_combined_blake3 {
        return Err(package_error(format!(
            "payload combined digest mismatch: manifest {} vs actual {combined}",
            manifest.payload_combined_blake3
        )));
    }
    if required_payload != manifest_payload {
        let missing = required_payload
            .difference(&manifest_payload)
            .map(RootRelativePath::as_str)
            .collect::<Vec<_>>();
        let extra = manifest_payload
            .difference(&required_payload)
            .map(RootRelativePath::as_str)
            .collect::<Vec<_>>();
        return Err(package_error(format!(
            "plan and manifest payload sets disagree (missing={missing:?}, extra={extra:?})"
        )));
    }
    let tar_set: std::collections::HashSet<_> = tar_payload.keys().cloned().collect();
    if tar_set != manifest_payload {
        let missing = manifest_payload
            .difference(&tar_set)
            .map(RootRelativePath::as_str)
            .collect::<Vec<_>>();
        let extra = tar_set
            .difference(&manifest_payload)
            .map(RootRelativePath::as_str)
            .collect::<Vec<_>>();
        return Err(package_error(format!(
            "manifest and tar payload sets disagree (missing={missing:?}, extra={extra:?})"
        )));
    }
    Ok(())
}
