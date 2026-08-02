//! Canonical streaming JSONL codec for immutable Compare artifacts.

use std::io::{BufRead, BufReader, Write};

use serde::{Deserialize, Serialize};
use syncdash::foundation::path::RootRelativePath;
use syncdash::fs::local_root::LocalRoot;
use syncdash::model::table::TableArtifact;

use super::super::model::result::{CompareResultVersion, RetainedPlan};
use super::artifact_validation::{validate_compare_result, validate_digest};
use super::error::invalid_data;
use super::integrity::require_schema;
use super::schema::{
    OperationRecord, OperationRecordRef, ResultFooter, ResultManifest, SnapshotEntryRecord,
    SnapshotEntryRecordRef,
};
use super::{RESULT_CHECKSUM_DOMAIN, STORE_SCHEMA};

pub(super) fn write_result_artifact(
    root: &LocalRoot,
    relative: &RootRelativePath,
    version: &CompareResultVersion,
    expected_checksum: &str,
) -> std::io::Result<()> {
    let mut staged = root.create_staged(relative)?;
    let checksum = write_result_body(&mut staged, version)?;
    if checksum != expected_checksum {
        return Err(invalid_data(
            "Compare-result streaming checksum changed between validation and publication",
        ));
    }
    write_json_line(
        &mut staged,
        &ResultFooter {
            record: "checksum".to_string(),
            checksum,
        },
    )?;
    staged.seal(true)?;
    staged.commit_noreplace()
}

pub(super) fn write_result_body(
    writer: &mut impl Write,
    version: &CompareResultVersion,
) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(RESULT_CHECKSUM_DOMAIN);
    let manifest = ResultManifest {
        schema: STORE_SCHEMA,
        record: "manifest".to_string(),
        identity: version.identity.clone(),
        plan_digest: version.plan_digest.clone(),
        plan_header: version.plan.header.clone(),
        compare_options: version.compare_options,
        identical_count: version.plan.identical_count,
        identical_bytes: version.plan.identical_bytes,
        operation_count: version.plan.operations.len() as u64,
        source_header: version.source.header.clone(),
        source_entry_count: version.source.entries.len() as u64,
        target_header: version.target.header.clone(),
        target_entry_count: version.target.entries.len() as u64,
    };
    write_hashed_json_line(writer, &mut hasher, &manifest)?;
    for (operation, metadata) in version
        .plan
        .operations
        .iter()
        .zip(version.plan.metadata.iter())
    {
        write_hashed_json_line(
            writer,
            &mut hasher,
            &OperationRecordRef {
                record: "operation",
                operation,
                metadata,
            },
        )?;
    }
    for entry in &version.source.entries {
        write_hashed_json_line(
            writer,
            &mut hasher,
            &SnapshotEntryRecordRef {
                record: "source_entry",
                entry,
            },
        )?;
    }
    for entry in &version.target.entries {
        write_hashed_json_line(
            writer,
            &mut hasher,
            &SnapshotEntryRecordRef {
                record: "target_entry",
                entry,
            },
        )?;
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(super) fn read_result_artifact(
    root: &LocalRoot,
    relative: &RootRelativePath,
    expected_checksum: Option<&str>,
) -> std::io::Result<CompareResultVersion> {
    let file = root.open_read(relative)?;
    let artifact_size = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    hasher.update(RESULT_CHECKSUM_DOMAIN);
    let manifest: ResultManifest = read_json_line(&mut reader, Some(&mut hasher), relative)?;
    require_schema(manifest.schema, "Compare-result artifact")?;
    require_record(&manifest.record, "manifest", relative)?;
    let operation_count = checked_record_count(
        manifest.operation_count,
        artifact_size,
        "operation",
        relative,
    )?;
    let source_entry_count = checked_record_count(
        manifest.source_entry_count,
        artifact_size,
        "source entry",
        relative,
    )?;
    let target_entry_count = checked_record_count(
        manifest.target_entry_count,
        artifact_size,
        "target entry",
        relative,
    )?;
    let mut operations = Vec::with_capacity(operation_count);
    let mut metadata = Vec::with_capacity(operation_count);
    for _ in 0..operation_count {
        let record: OperationRecord = read_json_line(&mut reader, Some(&mut hasher), relative)?;
        require_record(&record.record, "operation", relative)?;
        operations.push(record.operation);
        metadata.push(record.metadata);
    }
    let mut source_entries = Vec::with_capacity(source_entry_count);
    for _ in 0..source_entry_count {
        let record: SnapshotEntryRecord = read_json_line(&mut reader, Some(&mut hasher), relative)?;
        require_record(&record.record, "source_entry", relative)?;
        source_entries.push(record.entry);
    }
    let mut target_entries = Vec::with_capacity(target_entry_count);
    for _ in 0..target_entry_count {
        let record: SnapshotEntryRecord = read_json_line(&mut reader, Some(&mut hasher), relative)?;
        require_record(&record.record, "target_entry", relative)?;
        target_entries.push(record.entry);
    }
    let footer: ResultFooter = read_json_line(&mut reader, None, relative)?;
    require_record(&footer.record, "checksum", relative)?;
    validate_digest(&footer.checksum, "Compare-result artifact checksum")?;
    let computed_checksum = hasher.finalize().to_hex().to_string();
    if footer.checksum != computed_checksum {
        return Err(invalid_data(format!(
            "Compare artifact '{}' checksum is invalid",
            relative.as_str()
        )));
    }
    if expected_checksum.is_some_and(|expected| expected != computed_checksum) {
        return Err(invalid_data(format!(
            "Compare artifact '{}' checksum does not match its index record",
            relative.as_str()
        )));
    }
    let mut trailing = Vec::new();
    if reader.read_until(b'\n', &mut trailing)? != 0 {
        return Err(invalid_data(format!(
            "Compare artifact '{}' has records after its checksum footer",
            relative.as_str()
        )));
    }
    let version = CompareResultVersion {
        identity: manifest.identity,
        plan_digest: manifest.plan_digest,
        plan: RetainedPlan {
            header: manifest.plan_header,
            operations,
            metadata,
            identical_count: manifest.identical_count,
            identical_bytes: manifest.identical_bytes,
        },
        source: TableArtifact {
            header: manifest.source_header,
            entries: source_entries,
        },
        target: TableArtifact {
            header: manifest.target_header,
            entries: target_entries,
        },
        compare_options: manifest.compare_options,
    };
    validate_compare_result(&version)?;
    Ok(version)
}

fn write_hashed_json_line(
    writer: &mut impl Write,
    hasher: &mut blake3::Hasher,
    value: &impl Serialize,
) -> std::io::Result<()> {
    let mut hashing_writer = HashingWriter { writer, hasher };
    write_json_line(&mut hashing_writer, value)
}

fn write_json_line(writer: &mut impl Write, value: &impl Serialize) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)
        .map_err(|error| invalid_data(format!("cannot encode Compare-result record: {error}")))?;
    writer.write_all(b"\n")
}

fn read_json_line<T: for<'de> Deserialize<'de> + Serialize>(
    reader: &mut impl BufRead,
    hasher: Option<&mut blake3::Hasher>,
    relative: &RootRelativePath,
) -> std::io::Result<T> {
    let mut line = Vec::new();
    if reader.read_until(b'\n', &mut line)? == 0 {
        return Err(invalid_data(format!(
            "Compare artifact '{}' ended before all declared records were read",
            relative.as_str()
        )));
    }
    if !line.ends_with(b"\n") {
        return Err(invalid_data(format!(
            "Compare artifact '{}' contains an unterminated record",
            relative.as_str()
        )));
    }
    if let Some(hasher) = hasher {
        hasher.update(&line);
    }
    let value: T = serde_json::from_slice(&line).map_err(|error| {
        invalid_data(format!(
            "invalid Compare-result record in '{}': {error}",
            relative.as_str()
        ))
    })?;
    let mut canonical = serde_json::to_vec(&value).map_err(|error| {
        invalid_data(format!(
            "cannot canonicalize Compare-result record in '{}': {error}",
            relative.as_str()
        ))
    })?;
    canonical.push(b'\n');
    if canonical != line {
        return Err(invalid_data(format!(
            "Compare artifact '{}' contains a non-canonical or unknown record field",
            relative.as_str()
        )));
    }
    Ok(value)
}

fn checked_record_count(
    count: u64,
    artifact_size: u64,
    label: &str,
    relative: &RootRelativePath,
) -> std::io::Result<usize> {
    if count > artifact_size {
        return Err(invalid_data(format!(
            "Compare artifact '{}' declares more {label} records than its byte length can contain",
            relative.as_str()
        )));
    }
    usize::try_from(count).map_err(|_| {
        invalid_data(format!(
            "Compare artifact '{}' declares too many {label} records for this platform",
            relative.as_str()
        ))
    })
}

fn require_record(
    actual: &str,
    expected: &str,
    relative: &RootRelativePath,
) -> std::io::Result<()> {
    if actual != expected {
        return Err(invalid_data(format!(
            "Compare artifact '{}' contains record '{actual}' where '{expected}' is required",
            relative.as_str()
        )));
    }
    Ok(())
}

struct HashingWriter<'a, W> {
    writer: &'a mut W,
    hasher: &'a mut blake3::Hasher,
}

impl<W: Write> Write for HashingWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.writer.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}
