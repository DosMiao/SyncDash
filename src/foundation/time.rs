//! Time: read now, unix ms ↔ civil calendar. UTC, no chrono dependency.
//!
//! Rendering local time is the frontend's job; the data always carries `ts_ms`.

/// Wall clock, unix milliseconds. Returns 0 rather than panicking when the time is unavailable —
/// every caller is stamping or logging, and not one of them is worth aborting a sync over.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `SystemTime` → unix milliseconds.
pub fn systime_ms(t: std::time::SystemTime) -> i64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Metadata mtime → unix milliseconds (0 when the platform cannot say).
pub fn meta_mtime_ms(md: &std::fs::Metadata) -> i64 {
    md.modified().map(systime_ms).unwrap_or(0)
}

/// days since 1970-01-01 → (year, month, day). Howard Hinnant's `civil_from_days`.
///
/// `div_euclid`/`rem_euclid` rather than hand-written branches: negative days (before 1970) must
/// floor, and Rust's `/` truncates toward zero — using it directly turns 1969-12-31 into 1970-01-00.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Break out (year, month, day, hour, minute, second) (UTC).
fn parts(ms: i64) -> (i64, u32, u32, i64, i64, i64) {
    let secs = ms.div_euclid(1000);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    (y, m, d, sod / 3600, (sod % 3600) / 60, sod % 60)
}

/// `YYYYMMDD-HHMMSS` (UTC). For directory names and conflict-copy names: lexicographic order must come out chronological.
pub fn stamp_compact(ms: i64) -> String {
    let (y, m, d, h, mi, s) = parts(ms);
    format!("{y:04}{m:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// `YYYY-MM-DDTHH:MM:SSZ` (UTC). For CSV export and other places read by humans/Excel.
pub fn stamp_iso(ms: i64) -> String {
    let (y, m, d, h, mi, s) = parts(ms);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        // Negative days must floor — exactly the cell a hand-written `/` branch gets wrong
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // Leap day
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn stamp_matches_known_unix_times() {
        assert_eq!(stamp_compact(0), "19700101-000000");
        assert_eq!(stamp_compact(946_684_800_000), "20000101-000000");
        assert_eq!(stamp_compact(1_000_000_000_000), "20010909-014640");
        // Leap day
        assert_eq!(stamp_compact(951_782_400_000), "20000229-000000");
    }

    #[test]
    fn iso_and_compact_describe_the_same_instant() {
        let ms = 1_000_000_000_000;
        assert_eq!(stamp_compact(ms), "20010909-014640");
        assert_eq!(stamp_iso(ms), "2001-09-09T01:46:40Z");
    }

    #[test]
    fn stamps_sort_chronologically() {
        // Lexicographic order on directory names = chronological order; this is what lets runlog sort a listing without parsing
        let a = stamp_compact(1_000_000_000_000);
        let b = stamp_compact(1_000_000_001_000);
        assert!(a < b, "{a} !< {b}");
    }

    #[test]
    fn systime_epoch_is_zero() {
        assert_eq!(systime_ms(std::time::UNIX_EPOCH), 0);
    }
}
