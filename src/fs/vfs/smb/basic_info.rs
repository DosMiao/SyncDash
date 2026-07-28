//! FileBasicInformation (MS-FSCC 2.4.7) and the FILETIME arithmetic around it.
//!
//! Kept apart from the wire code because this is where an mtime quietly becomes the wrong
//! instant: two epochs, two units, and a zero that does not mean zero. All of it is pure,
//! so it is pinned by unit tests that need no server.

/// 100-ns ticks between the Windows epoch (1601-01-01) and the Unix epoch (1970-01-01).
const EPOCH_DIFF_100NS: i64 = 116_444_736_000_000_000;

/// A FILETIME tick is 100 ns, so a millisecond is ten thousand of them.
const TICKS_PER_MS: i64 = 10_000;

/// The buffer is four 8-byte times, then two 4-byte words.
const FILE_BASIC_INFORMATION_LEN: usize = 40;

/// FileBasicInformation, from the SMB2 file-information-class table.
pub const FILE_BASIC_INFORMATION: u8 = 4;

/// Unix milliseconds → FILETIME ticks.
///
/// Never returns 0. MS-FSCC reads a zero time field as "leave this one alone", so a caller
/// asking for 1601-01-01 — or any earlier instant, which the wire cannot carry at all —
/// would get a silent no-op where it asked for a write. Clamping to one tick keeps the
/// write real, at a cost of 100 ns on a date no filesystem in this tool's path stores.
pub fn filetime_from_unix_ms(ms: i64) -> u64 {
    let ticks = ms
        .checked_mul(TICKS_PER_MS)
        .and_then(|t| t.checked_add(EPOCH_DIFF_100NS))
        .unwrap_or(i64::MAX);
    ticks.max(1) as u64
}

/// FILETIME ticks → Unix milliseconds.
///
/// Floors via `div_euclid`, matching `local`'s mtime handling: plain `/` truncates toward
/// zero, which drags pre-1970 times a millisecond *forward* and makes the round trip
/// asymmetric exactly where it is hardest to notice.
pub fn unix_ms_from_filetime(ticks: u64) -> i64 {
    let t = i64::try_from(ticks).unwrap_or(i64::MAX);
    (t - EPOCH_DIFF_100NS).div_euclid(TICKS_PER_MS)
}

/// The 40-byte FileBasicInformation buffer that sets the write time and nothing else.
///
/// The three times we are not setting go out as zero, which MS-FSCC defines as "no change"
/// — the same rule that forces `filetime_from_unix_ms` never to produce a zero. FileAttributes
/// gets a zero for the same reason: there it means "leave the attributes as they are", not
/// "clear them", so this cannot strip read-only or hidden off a file as a side effect.
///
/// Measured against macOS 15's smbd rather than taken on faith, because the alternative — read
/// the other three with `stat` and write them back — is the obvious "fix" and it is the wrong
/// one. Moving a write time *forward* leaves creation and access times byte-identical, so the
/// zero sentinel is genuinely honoured. Moving one *backward*, before the file's birth time,
/// drags the creation time down with it — but that is APFS enforcing birth ≤ modification
/// underneath the protocol, and it applies just as much to a creation time written back
/// explicitly. Sending the old value would not preserve it; it would only turn a field we
/// declined to touch into one we actively asserted and lost.
pub fn set_write_time_buffer(ticks: u64) -> Vec<u8> {
    let mut b = Vec::with_capacity(FILE_BASIC_INFORMATION_LEN);
    b.extend_from_slice(&0u64.to_le_bytes()); // CreationTime   — unchanged
    b.extend_from_slice(&0u64.to_le_bytes()); // LastAccessTime — unchanged
    b.extend_from_slice(&ticks.to_le_bytes()); // LastWriteTime — the one that matters
    b.extend_from_slice(&0u64.to_le_bytes()); // ChangeTime     — unchanged
    b.extend_from_slice(&0u32.to_le_bytes()); // FileAttributes — 0 = no change
    b.extend_from_slice(&0u32.to_le_bytes()); // Reserved
    debug_assert_eq!(b.len(), FILE_BASIC_INFORMATION_LEN);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The anchor the whole conversion hangs on, checked against a value computable by hand:
    /// 2024-01-01T00:00:00Z is unix 1_704_067_200 s, so 1_704_067_200 × 10^7 ticks after the
    /// Unix epoch, plus the epoch difference.
    #[test]
    fn known_instant_matches_hand_arithmetic() {
        let ms = 1_704_067_200_000;
        let ticks = filetime_from_unix_ms(ms);
        assert_eq!(ticks, 133_485_408_000_000_000);
        assert_eq!(unix_ms_from_filetime(ticks), ms);
    }

    #[test]
    fn roundtrips_at_millisecond_resolution() {
        // The value the conformance suite actually writes, sub-second digits and all.
        for ms in [0, 1, 1_600_000_123_456, 1_704_067_200_123, 253_402_300_799_000] {
            assert_eq!(unix_ms_from_filetime(filetime_from_unix_ms(ms)), ms, "ms={ms}");
        }
    }

    /// Pre-1970 mtimes exist on real archives, and this is the case where a truncating
    /// division silently shifts the instant instead of failing.
    #[test]
    fn pre_unix_epoch_times_floor_rather_than_truncate() {
        let ms = -1; // 1969-12-31T23:59:59.999Z
        let ticks = filetime_from_unix_ms(ms);
        assert_eq!(ticks, (EPOCH_DIFF_100NS - TICKS_PER_MS) as u64);
        assert_eq!(unix_ms_from_filetime(ticks), ms);
        // A sub-tick remainder must floor down, never toward zero.
        assert_eq!(unix_ms_from_filetime((EPOCH_DIFF_100NS - 1) as u64), -1);
    }

    /// A zero on the wire means "leave unchanged", so the encoder must never emit one for a
    /// time the caller actually asked to set.
    #[test]
    fn no_requested_time_ever_encodes_as_the_leave_unchanged_sentinel() {
        // Exactly the Windows epoch, and well before it: both would compute to <= 0 ticks.
        for ms in [-11_644_473_600_000, -999_999_999_999_999, i64::MIN] {
            assert!(filetime_from_unix_ms(ms) >= 1, "ms={ms} encoded as the no-change sentinel");
        }
    }

    #[test]
    fn buffer_is_forty_bytes_with_only_the_write_time_set() {
        let ticks = filetime_from_unix_ms(1_600_000_123_456);
        let b = set_write_time_buffer(ticks);
        assert_eq!(b.len(), FILE_BASIC_INFORMATION_LEN);
        assert_eq!(&b[0..8], &[0u8; 8], "creation time must say 'unchanged'");
        assert_eq!(&b[8..16], &[0u8; 8], "access time must say 'unchanged'");
        assert_eq!(u64::from_le_bytes(b[16..24].try_into().unwrap()), ticks);
        assert_eq!(&b[24..32], &[0u8; 8], "change time must say 'unchanged'");
        assert_eq!(&b[32..36], &[0u8; 4], "attributes must say 'unchanged', not 'cleared'");
    }

    /// Absurd inputs must saturate rather than wrap into a plausible-looking date.
    #[test]
    fn out_of_range_input_saturates() {
        assert_eq!(filetime_from_unix_ms(i64::MAX), i64::MAX as u64);
        assert_eq!(unix_ms_from_filetime(u64::MAX), (i64::MAX - EPOCH_DIFF_100NS) / TICKS_PER_MS);
    }
}
