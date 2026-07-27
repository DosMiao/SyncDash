//! Human-facing number formatting.
//!
//! Units are KiB/MiB/GiB (base 1024, named for base 1024). The frontend once paired KB/MB/GB with a
//! 1024 divisor, so the same number read differently in two places — this module is the source of truth.

/// Byte count → `1.5 MiB`. Under 1 KiB there are no decimals, just `938 B`.
pub fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

/// Percentage, 0..=100. Returns 0 rather than NaN/divide-by-zero when the denominator is 0.
///
/// Before consolidation this expression was written out in 6 places, and **each had a different
/// zero-denominator fallback**: `src-tauri` fell back to -1, the CLI to 100, the frontend to 0.
/// Standardised on 0: "no total" should read as "not started yet", not "complete".
pub fn pct(done: u64, total: u64) -> u64 {
    if total == 0 { 0 } else { (done.min(total) * 100) / total }
}

/// Milliseconds → `1:02:03` / `2:03` / `3s`. For run durations.
pub fn human_duration(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{sec:02}") }
    else if m > 0 { format!("{m}:{sec:02}") }
    else { format!("{sec}s") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_use_binary_units_and_drop_decimals_under_1k() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(938), "938 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024 * 3 / 2), "1.5 MiB");
        assert_eq!(human_bytes(1024u64.pow(4)), "1.0 TiB");
        // Caps at TiB, never rolls off the end of the unit table
        assert_eq!(human_bytes(1024u64.pow(5)), "1024.0 TiB");
    }

    #[test]
    fn pct_saturates_and_never_divides_by_zero() {
        assert_eq!(pct(0, 0), 0, "no total means 0% (not started yet), not 100%");
        assert_eq!(pct(5, 0), 0);
        assert_eq!(pct(1, 4), 25);
        assert_eq!(pct(4, 4), 100);
        assert_eq!(pct(9, 4), 100, "done above total is capped; must not report 225%");
    }

    #[test]
    fn duration_picks_the_coarsest_useful_shape() {
        assert_eq!(human_duration(0), "0s");
        assert_eq!(human_duration(3_000), "3s");
        assert_eq!(human_duration(123_000), "2:03");
        assert_eq!(human_duration(3_723_000), "1:02:03");
        assert_eq!(human_duration(-5), "0s", "a negative duration (clock rollback) must not display as negative");
    }
}
