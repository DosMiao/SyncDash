//! Conflict-copy identity and naming rules.

use crate::foundation::names::CONFLICT_INFIX;
use crate::foundation::path::{base_name, split_ext, split_parent};
use crate::foundation::text::safe_host;
use crate::foundation::time::stamp_compact;

/// Conflict-copy name: `report.pdf` → `report.sync-conflict-20260726-143000-WIN01.pdf`.
pub fn conflict_name(path: &str, host: &str, at_ms: u64) -> String {
    let (dir, base) = split_parent(path);
    // Hidden files count wholly as the stem; only a later dot introduces an extension.
    let (stem, ext) = split_ext(base);
    let ts = stamp_compact(at_ms as i64);
    let host = safe_host(host);
    format!("{dir}{stem}{CONFLICT_INFIX}{ts}-{host}{ext}")
}

/// Conflict copies never participate in conflict decisions.
pub fn is_conflict_copy(path: &str) -> bool {
    base_name(path).contains(CONFLICT_INFIX)
}
