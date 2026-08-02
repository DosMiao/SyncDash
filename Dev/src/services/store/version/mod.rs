//! Optional in-root history for deleted and overwritten files.
//!
//! With `versioning = true`, preserved content lives in that root's `.version_syncDash/`, so the
//! history remains available from either machine that can access the root.
//!
//! Directory layout:
//!   <root>/.version_syncDash/
//!     index.jsonl                  one line per version {id, ts_ms, host, ops, preserved, bytes}
//!     <id>/plan.jsonl              the op list this run executed (audit trail)
//!     <id>/manifest.json           preserved entries: rel → whole|rdelta + the hashes + original mtime/mode
//!     <id>/files/<rel>             whole-file copy of the original (small files and deleted files)
//!     <id>/rdelta/<rel>            reverse-patch blob (overwritten files ≥4MB whose new content can serve as a reference)
//!
//! FastCDC reverse patches express the old file as chunks from the current file plus old-only blob
//! chunks. Restore verifies the current base hash and the reconstructed old hash.

mod content;
mod model;
mod restore;
mod retention;
mod writer;

#[cfg(test)]
mod tests;

pub use model::{PreservedEntry, VersionIndexEntry, VersionManifest, VersionPayloadKind};
pub use restore::restore;
pub use retention::{list, prune};
pub use writer::VersionWriter;
