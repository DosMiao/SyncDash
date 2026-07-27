//! 时间：取当下、unix ms ↔ 民用历法。UTC，不引 chrono。
//!
//! 本地时间的显示归前端，数据里始终是 `ts_ms`。

/// 墙上时钟，unix 毫秒。取不到时间时返回 0（而不是 panic）——
/// 调用方全都是在打标签/记日志，没有一个值得为此中断同步。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `SystemTime` → unix 毫秒。
pub fn systime_ms(t: std::time::SystemTime) -> i64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// days since 1970-01-01 → (year, month, day)。Howard Hinnant 的 `civil_from_days`。
///
/// 用 `div_euclid`/`rem_euclid` 而非手写分支：负数天（1970 之前）必须向下取整，
/// Rust 的 `/` 是向零截断，直接用会让 1969-12-31 算成 1970-01-00。
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

/// 拆出 (年, 月, 日, 时, 分, 秒)（UTC）。
fn parts(ms: i64) -> (i64, u32, u32, i64, i64, i64) {
    let secs = ms.div_euclid(1000);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    (y, m, d, sod / 3600, (sod % 3600) / 60, sod % 60)
}

/// `YYYYMMDD-HHMMSS`（UTC）。用于目录名与冲突副本名：要能按字典序排出时间序。
pub fn stamp_compact(ms: i64) -> String {
    let (y, m, d, h, mi, s) = parts(ms);
    format!("{y:04}{m:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// `YYYY-MM-DDTHH:MM:SSZ`（UTC）。用于 CSV 导出等要给人/Excel 看的地方。
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
        // 负数天必须向下取整——这正是手写 `/` 分支会错的那一格
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // 闰日
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn stamp_matches_known_unix_times() {
        assert_eq!(stamp_compact(0), "19700101-000000");
        assert_eq!(stamp_compact(946_684_800_000), "20000101-000000");
        assert_eq!(stamp_compact(1_000_000_000_000), "20010909-014640");
        // 闰日
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
        // 目录名按字典序排 = 按时间排，这是 runlog 列目录时不做解析就能排序的前提
        let a = stamp_compact(1_000_000_000_000);
        let b = stamp_compact(1_000_000_001_000);
        assert!(a < b, "{a} !< {b}");
    }

    #[test]
    fn systime_epoch_is_zero() {
        assert_eq!(systime_ms(std::time::UNIX_EPOCH), 0);
    }
}
