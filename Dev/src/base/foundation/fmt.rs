//! Human-facing rendering shared by every command that reports what a run did.
//!
//! Units are KiB/MiB/GiB (base 1024, named for base 1024).
//! `Dev/typescript/core/shared/format.ts` mirrors this
//! rendering for the GUI; the two must agree, or one byte count reads two ways.
//! This module is the arbiter of that agreement, including how the tenth is rounded, so the tie
//! cases below are pinned rather than inherited silently from `format!`.

/// What a command reports in place of its action verb when nothing was written.
///
/// Dry-run is the default for every writing command, so the same sentence has to name the same
/// escape hatch everywhere: a summary that says "dry-run" without naming `--apply` reads as a
/// failure, and one that names a flag the command does not have is worse.
pub const DRY_RUN_HINT: &str = "dry-run (rerun with --apply)";

/// Byte count → `1.5 MiB`. Under 1 KiB there are no decimals, just `938 B`.
pub fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
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

    /// `n / 1024^k` is dyadic, so an exact tie at one decimal really occurs — at `.25` and `.75`,
    /// the only two dyadic tie fractions. `{:.1}` breaks them to the even tenth. The GUI mirror
    /// pins the same values, so a change here has to be a deliberate change on both sides.
    #[test]
    fn an_exact_tenth_tie_rounds_to_the_even_tenth() {
        assert_eq!(human_bytes(1280), "1.2 KiB");
        assert_eq!(human_bytes(1792), "1.8 KiB");
        assert_eq!(human_bytes(1024 * 1024 * 5 / 4), "1.2 MiB");
    }
}
