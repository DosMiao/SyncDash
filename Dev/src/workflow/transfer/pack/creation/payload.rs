//! Streaming whole-file and FastCDC-delta payload readers with Compare-evidence fencing.

use std::io::Read;

use crate::model::plan::Op;
use crate::transfer::pack::format::package_error;

pub(super) struct SourceEvidence {
    pub(super) size: u64,
    pub(super) full_hash: String,
    pub(super) sampled_hash: String,
}

pub(super) struct EvidenceReader<R: Read> {
    inner: R,
    full_hash: blake3::Hasher,
    sampled_hash: crate::pipeline::scan::digest::SampledDigestBuilder,
    offset: u64,
}

impl<R: Read> EvidenceReader<R> {
    pub(super) fn new(inner: R, expected_size: u64) -> Self {
        Self {
            inner,
            full_hash: blake3::Hasher::new(),
            sampled_hash: crate::pipeline::scan::digest::SampledDigestBuilder::new(expected_size),
            offset: 0,
        }
    }

    pub(super) fn finish(self) -> SourceEvidence {
        SourceEvidence {
            size: self.offset,
            full_hash: self.full_hash.finalize().to_hex().to_string(),
            sampled_hash: format!("~{}", self.sampled_hash.finish()),
        }
    }
}

impl<R: Read> Read for EvidenceReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let length = self.inner.read(buffer)?;
        self.full_hash.update(&buffer[..length]);
        self.sampled_hash.update(self.offset, &buffer[..length]);
        self.offset = self
            .offset
            .checked_add(length as u64)
            .ok_or_else(|| package_error("source file length overflow"))?;
        Ok(length)
    }
}

pub(super) struct DeltaBlobReader<'base, R: Read> {
    chunks: crate::fs::chunk::ChunkStream<R>,
    base_chunks: &'base std::collections::HashMap<(&'base str, u32), u64>,
    expected_blob_size: u64,
    current: Vec<u8>,
    current_offset: usize,
    file_size: u64,
    file_hash: blake3::Hasher,
    blob_size: u64,
    blob_hash: blake3::Hasher,
    finished: bool,
}

impl<'base, R: Read> DeltaBlobReader<'base, R> {
    pub(super) fn new(
        reader: R,
        base_chunks: &'base std::collections::HashMap<(&'base str, u32), u64>,
        expected_blob_size: u64,
    ) -> Self {
        Self {
            chunks: crate::fs::chunk::stream_chunks(reader),
            base_chunks,
            expected_blob_size,
            current: Vec::new(),
            current_offset: 0,
            file_size: 0,
            file_hash: blake3::Hasher::new(),
            blob_size: 0,
            blob_hash: blake3::Hasher::new(),
            finished: false,
        }
    }

    pub(super) fn verify(
        self,
        expected_file_size: u64,
        expected_file_hash: &str,
        expected_blob_hash: &str,
    ) -> std::io::Result<()> {
        if !self.finished
            || self.file_size != expected_file_size
            || self.file_hash.finalize().to_hex().as_str() != expected_file_hash
        {
            return Err(package_error(
                "source file changed while its delta payload was being packed",
            ));
        }
        if self.blob_size != self.expected_blob_size
            || self.blob_hash.finalize().to_hex().as_str() != expected_blob_hash
        {
            return Err(package_error(
                "source file changed which chunks belong in its delta payload",
            ));
        }
        Ok(())
    }
}

impl<R: Read> Read for DeltaBlobReader<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            if self.current_offset < self.current.len() {
                let available = &self.current[self.current_offset..];
                let length = available.len().min(output.len());
                output[..length].copy_from_slice(&available[..length]);
                self.current_offset += length;
                return Ok(length);
            }
            match self.chunks.next() {
                Some(Ok(chunk)) => {
                    self.file_size = self
                        .file_size
                        .checked_add(chunk.info.len as u64)
                        .ok_or_else(|| package_error("source file length overflow"))?;
                    self.file_hash.update(&chunk.bytes);
                    if self
                        .base_chunks
                        .contains_key(&(chunk.info.hash.as_str(), chunk.info.len))
                    {
                        continue;
                    }
                    let blob_size = self
                        .blob_size
                        .checked_add(chunk.info.len as u64)
                        .ok_or_else(|| package_error("delta blob length overflow"))?;
                    if blob_size > self.expected_blob_size {
                        return Err(package_error(
                            "source file gained unmatched chunks while its delta payload was being packed",
                        ));
                    }
                    self.blob_size = blob_size;
                    self.blob_hash.update(&chunk.bytes);
                    self.current = chunk.bytes;
                    self.current_offset = 0;
                }
                Some(Err(error)) => return Err(error),
                None => {
                    self.finished = true;
                    return Ok(0);
                }
            }
        }
    }
}

pub(super) fn verify_source_evidence(
    operation: &Op,
    size: u64,
    mtime_ms: i64,
    mode: Option<u32>,
    evidence: &SourceEvidence,
) -> std::io::Result<()> {
    if evidence.size != size || operation.size.is_some_and(|expected| expected != size) {
        return Err(package_error(format!(
            "source file changed size after Compare: {}",
            operation.path
        )));
    }
    if operation
        .mode
        .is_some_and(|expected| mode != Some(expected))
    {
        return Err(package_error(format!(
            "source file changed permissions after Compare: {}",
            operation.path
        )));
    }
    match operation.hash.as_deref() {
        Some(expected) if crate::model::digest::is_sampled_digest(expected) => {
            if evidence.sampled_hash != expected {
                return Err(package_error(format!(
                    "source file content changed after Compare: {}",
                    operation.path
                )));
            }
        }
        Some(expected) if expected != evidence.full_hash => {
            return Err(package_error(format!(
                "source file content changed after Compare: {}",
                operation.path
            )));
        }
        Some(_) => {}
        None => {}
    }
    if operation
        .mtime_ms
        .is_some_and(|expected| expected != mtime_ms)
    {
        return Err(package_error(format!(
            "source file timestamp changed after Compare: {}",
            operation.path
        )));
    }
    Ok(())
}
