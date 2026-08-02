//! FastCDC content-defined chunking over bytes or a retained local file.
//!
//! Both endpoints use the fixed 16K/64K/256K v2020 parameters. File chunking streams through a
//! bounded buffer while computing the whole-file digest in the same pass.

use crate::foundation::path::RootRelativePath;
use crate::fs::local_root::LocalRoot;
use crate::model::chunk::{ChunkInfo, FileChunks};

const MIN: u32 = 16 * 1024;
const AVG: u32 = 64 * 1024;
const MAX: u32 = 256 * 1024;

pub struct ChunkStreamSummary {
    pub size: u64,
    pub hash: String,
}

pub(crate) struct StreamedChunk {
    pub info: ChunkInfo,
    pub bytes: Vec<u8>,
}

pub(crate) struct ChunkStream<R: std::io::Read> {
    inner: fastcdc::v2020::StreamCDC<R>,
}

impl<R: std::io::Read> Iterator for ChunkStream<R> {
    type Item = std::io::Result<StreamedChunk>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|result| {
            let chunk = result.map_err(std::io::Error::from)?;
            let info = ChunkInfo {
                off: chunk.offset,
                len: u32::try_from(chunk.length).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "chunk length overflow")
                })?,
                hash: blake3::hash(&chunk.data).to_hex().to_string(),
            };
            Ok(StreamedChunk {
                info,
                bytes: chunk.data,
            })
        })
    }
}

pub(crate) fn stream_chunks<R: std::io::Read>(reader: R) -> ChunkStream<R> {
    ChunkStream {
        inner: fastcdc::v2020::StreamCDC::new(reader, MIN, AVG, MAX),
    }
}

pub fn chunk_bytes(data: &[u8]) -> Vec<ChunkInfo> {
    collect_chunks(std::io::Cursor::new(data))
        .expect("reading from an in-memory byte slice cannot fail")
        .0
}

pub fn chunk_file(root: &LocalRoot, rel: &RootRelativePath) -> std::io::Result<FileChunks> {
    let (chunks, summary) = collect_chunks(root.open_read(rel)?)?;
    Ok(FileChunks {
        rel: rel.clone(),
        size: summary.size,
        hash: summary.hash,
        chunks,
    })
}

pub fn visit_chunks(
    reader: impl std::io::Read,
    mut visitor: impl FnMut(&ChunkInfo, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<ChunkStreamSummary> {
    let mut file_hash = blake3::Hasher::new();
    let mut size = 0u64;
    for chunk in stream_chunks(reader) {
        let chunk = chunk?;
        file_hash.update(&chunk.bytes);
        size = size.checked_add(chunk.info.len as u64).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "file length overflow")
        })?;
        visitor(&chunk.info, &chunk.bytes)?;
    }
    Ok(ChunkStreamSummary {
        size,
        hash: file_hash.finalize().to_hex().to_string(),
    })
}

fn collect_chunks(
    reader: impl std::io::Read,
) -> std::io::Result<(Vec<ChunkInfo>, ChunkStreamSummary)> {
    let mut chunks = Vec::new();
    let summary = visit_chunks(reader, |chunk, _| {
        chunks.push(chunk.clone());
        Ok(())
    })?;
    Ok((chunks, summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunking_is_deterministic_for_the_same_bytes() {
        let data: Vec<u8> = (0..200_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let first = chunk_bytes(&data);
        let second = chunk_bytes(&data);
        assert!(!first.is_empty(), "a 200KB buffer must produce chunks");
        assert_eq!(first.len(), second.len());
        for (left, right) in first.iter().zip(second.iter()) {
            assert_eq!(left.hash, right.hash);
            assert_eq!((left.off, left.len), (right.off, right.len));
        }
    }

    #[test]
    fn streaming_matches_the_slice_fastcdc_contract() {
        let data: Vec<u8> = (0..900_000u32)
            .map(|i| (i.wrapping_mul(1_664_525).wrapping_add(1_013_904_223) >> 9) as u8)
            .collect();
        let expected: Vec<ChunkInfo> = fastcdc::v2020::FastCDC::new(&data, MIN, AVG, MAX)
            .map(|chunk| ChunkInfo {
                off: chunk.offset as u64,
                len: chunk.length as u32,
                hash: blake3::hash(&data[chunk.offset..chunk.offset + chunk.length])
                    .to_hex()
                    .to_string(),
            })
            .collect();
        let actual = chunk_bytes(&data);

        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected.iter()) {
            assert_eq!(actual.off, expected.off);
            assert_eq!(actual.len, expected.len);
            assert_eq!(actual.hash, expected.hash);
        }
    }

    #[test]
    fn chunks_tile_the_input_without_gap_or_overlap() {
        let data: Vec<u8> = (0..150_000u32).map(|i| (i % 251) as u8).collect();
        let chunks = chunk_bytes(&data);
        let mut offset = 0u64;
        for chunk in &chunks {
            assert_eq!(
                chunk.off, offset,
                "chunk starts where the previous one ended"
            );
            offset += chunk.len as u64;
        }
        assert_eq!(
            offset,
            data.len() as u64,
            "the chunks together must cover the whole input"
        );
    }

    #[test]
    fn an_edit_disturbs_only_nearby_boundaries() {
        let data: Vec<u8> = (0..300_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8)
            .collect();
        let mut edited = data.clone();
        for byte in edited.iter_mut().skip(150_000).take(64) {
            *byte ^= 0xFF;
        }
        let original_chunks = chunk_bytes(&data);
        let edited_chunks = chunk_bytes(&edited);
        let shared = original_chunks
            .iter()
            .filter(|chunk| edited_chunks.iter().any(|edited| edited.hash == chunk.hash))
            .count();
        assert!(
            shared * 2 >= original_chunks.len(),
            "a 64-byte edit should leave over half of {} chunks reusable, kept {shared}",
            original_chunks.len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn chunking_stays_with_the_retained_root_after_a_name_swap() {
        use std::os::unix::fs::symlink;

        let base =
            std::env::temp_dir().join(format!("syncdash-chunk-root-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let selected = base.join("selected");
        let detached = base.join("detached");
        let outside = base.join("outside");
        std::fs::create_dir_all(&selected).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(selected.join("file.bin"), b"inside").unwrap();
        std::fs::write(outside.join("file.bin"), b"outside").unwrap();
        let root = LocalRoot::open(selected.clone()).unwrap();

        std::fs::rename(&selected, &detached).unwrap();
        symlink(&outside, &selected).unwrap();
        let chunks = chunk_file(&root, &RootRelativePath::try_from("file.bin").unwrap()).unwrap();

        assert_eq!(chunks.hash, blake3::hash(b"inside").to_hex().to_string());
        assert_eq!(chunks.size, b"inside".len() as u64);
        let _ = std::fs::remove_dir_all(base);
    }
}
