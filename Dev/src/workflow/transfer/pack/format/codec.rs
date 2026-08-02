//! First-pass tar decoding and complete metadata/digest validation.

use std::io::Read;

use crate::foundation::path::RootRelativePath;
use crate::model::plan::Plan;
use crate::transfer::pack::format::{package_error, DecodedPackage, Manifest, PACK_VERSION};

use crate::transfer::pack::format::validation::validate_package_structure;

/// Decode and validate every package member before the application branch creates staging state.
/// The checks deliberately remain in protocol order because callers and tests rely on the first
/// safety failure being the most immediate malformed evidence.
pub(in crate::transfer::pack) fn decode_and_validate(
    package_file: &mut std::fs::File,
) -> std::io::Result<DecodedPackage> {
    let mut plan_bytes: Option<Vec<u8>> = None;
    let mut manifest: Option<Manifest> = None;
    let mut tar_payload = std::collections::HashMap::new();
    {
        let mut archive = tar::Archive::new(std::io::BufReader::new(&mut *package_file));
        for entry in archive.entries()? {
            let mut entry = entry?;
            if !entry.header().entry_type().is_file() {
                return Err(package_error("package members must all be regular files"));
            }
            let path = entry.path()?;
            let name = path.to_str().ok_or_else(|| {
                package_error("package contains a member whose path is not valid UTF-8")
            })?;
            if name == "plan.jsonl" {
                if plan_bytes.is_some() {
                    return Err(package_error(
                        "package contains duplicate plan.jsonl members",
                    ));
                }
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes)?;
                plan_bytes = Some(bytes);
            } else if name == "manifest.json" {
                if manifest.is_some() {
                    return Err(package_error(
                        "package contains duplicate manifest.json members",
                    ));
                }
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes)?;
                manifest = Some(
                    serde_json::from_slice(&bytes)
                        .map_err(|error| package_error(format!("bad manifest: {error}")))?,
                );
            } else if let Some(relative) = name.strip_prefix("payload/") {
                let relative = RootRelativePath::try_from(relative)
                    .map_err(|error| package_error(format!("unsafe tar payload path: {error}")))?;
                let size = entry.size();
                if tar_payload.insert(relative.clone(), size).is_some() {
                    return Err(package_error(format!(
                        "package contains duplicate payload {:?}",
                        relative.as_str()
                    )));
                }
            } else {
                return Err(package_error(format!(
                    "package contains unexpected member {name:?}"
                )));
            }
        }
    }

    let plan_bytes = plan_bytes.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "package has no plan.jsonl")
    })?;
    let manifest = manifest.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "package has no manifest.json",
        )
    })?;

    if manifest.pack_version > PACK_VERSION {
        return Err(package_error(format!(
            "package version {} newer than this binary supports ({PACK_VERSION}) — rebuild the peer",
            manifest.pack_version
        )));
    }
    if manifest.pack_version < PACK_VERSION {
        return Err(package_error(format!(
            "package version {} is older than this binary requires ({PACK_VERSION}) — regenerate the package",
            manifest.pack_version
        )));
    }
    let actual_plan_hash = blake3::hash(&plan_bytes).to_hex().to_string();
    if actual_plan_hash != manifest.plan_blake3 {
        return Err(package_error(format!(
            "plan hash mismatch: manifest {} vs actual {actual_plan_hash}",
            manifest.plan_blake3
        )));
    }

    let plan = Plan::from_reader(std::io::BufReader::new(&plan_bytes[..]))?;
    validate_package_structure(&plan, &manifest, &tar_payload)?;
    Ok(DecodedPackage { plan, manifest })
}
