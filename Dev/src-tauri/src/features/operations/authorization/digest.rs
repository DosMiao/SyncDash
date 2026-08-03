//! Stable digests binding reviews to exact decisions and health facts.

use crate::contracts::compare::ReviewedRowDecisionDto;

pub(super) fn reviewed_row_decisions_digest(
    reviewed_row_decisions: &[ReviewedRowDecisionDto],
) -> Result<String, String> {
    let mut normalized: Vec<(usize, bool)> = reviewed_row_decisions
        .iter()
        .map(|decision| (decision.index, decision.direction_reversed))
        .collect();
    normalized.sort_unstable();
    if normalized.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err("The reviewed row decisions contain a duplicate index".into());
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"syncdash-reviewed-row-decisions-v1\0");
    for (index, direction_reversed) in normalized {
        hasher.update(&(index as u64).to_le_bytes());
        hasher.update(&[u8::from(direction_reversed)]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(crate) fn health_review_digest(verdict: &syncdash::pipeline::guard::Verdict) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"syncdash-health-review-v2\0");
    hash_messages(&mut hasher, b"blockers", &verdict.blockers);
    hash_messages(&mut hasher, b"warnings", &verdict.warnings);
    hasher.finalize().to_hex().to_string()
}

fn hash_messages(hasher: &mut blake3::Hasher, label: &[u8], messages: &[String]) {
    hasher.update(&(label.len() as u64).to_le_bytes());
    hasher.update(label);
    let mut normalized = messages.to_vec();
    normalized.sort();
    for message in normalized {
        hasher.update(&(message.len() as u64).to_le_bytes());
        hasher.update(message.as_bytes());
    }
}
