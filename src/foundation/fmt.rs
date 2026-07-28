//! Human-facing number formatting.
//!
//! Units are KiB/MiB/GiB (base 1024, named for base 1024). `typescript/core/format.ts` mirrors this
//! rendering for the GUI; the two must agree, or one byte count reads two ways.

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
}
