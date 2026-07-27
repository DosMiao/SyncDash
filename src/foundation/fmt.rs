//! 给人看的数字格式化。
//!
//! 单位口径 KiB/MiB/GiB（1024 进制，名字也用 1024 的名字）。前端曾用 KB/MB/GB 配
//! 1024 除数，同一个数字两处显示不一致——以此处为准。

/// 字节数 → `1.5 MiB`。小于 1 KiB 时不带小数，直接 `938 B`。
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

/// 百分比，0..=100。分母为 0 时返回 0 而不是 NaN/除零。
///
/// 合并前这个式子在 6 处各写了一遍，且**分母为 0 时的回退各不相同**：
/// `src-tauri` 回退到 -1，CLI 回退到 100，前端回退到 0。统一成 0：
/// "没有总量"应该显示"还没开始"，不是"已完成"。
pub fn pct(done: u64, total: u64) -> u64 {
    if total == 0 { 0 } else { (done.min(total) * 100) / total }
}

/// 毫秒 → `1:02:03` / `2:03` / `3s`。给运行时长用。
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
        // 封顶在 TiB，不会滚出单位表
        assert_eq!(human_bytes(1024u64.pow(5)), "1024.0 TiB");
    }

    #[test]
    fn pct_saturates_and_never_divides_by_zero() {
        assert_eq!(pct(0, 0), 0, "没有总量时是 0%（还没开始），不是 100%");
        assert_eq!(pct(5, 0), 0);
        assert_eq!(pct(1, 4), 25);
        assert_eq!(pct(4, 4), 100);
        assert_eq!(pct(9, 4), 100, "done 超过 total 时封顶，不能报 225%");
    }

    #[test]
    fn duration_picks_the_coarsest_useful_shape() {
        assert_eq!(human_duration(0), "0s");
        assert_eq!(human_duration(3_000), "3s");
        assert_eq!(human_duration(123_000), "2:03");
        assert_eq!(human_duration(3_723_000), "1:02:03");
        assert_eq!(human_duration(-5), "0s", "负时长（时钟回拨）不能显示成负数");
    }
}
