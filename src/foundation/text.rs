//! Path text normalization: the repo's one and only comparison-key implementation.
//!
//! Folding = NFC plus (optionally) case. NFC cannot be skipped: macOS/APFS writes NFD by default, and
//! without that step the same name is not the same name to compare and to filter — an NFD path slips
//! out from under the exclude mask.

use unicode_normalization::UnicodeNormalization;

/// Comparison key: NFC normalization plus (optionally) case-folding. **Matching only, never I/O** —
/// always write the original string to disk, or you create a renamed file on the far side.
pub fn norm_key(p: &str, case_insensitive: bool) -> String {
    let nfc: String = p.nfc().collect();
    if case_insensitive { nfc.to_uppercase() } else { nfc }
}

/// Folding for the case-insensitive case (equivalent to `norm_key(p, true)`).
pub fn fold(p: &str) -> String {
    norm_key(p, true)
}

/// Hostname → a form safe inside a filename (anything that is not alphanumeric or `-` becomes `-`).
pub fn safe_host(host: &str) -> String {
    host.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two spellings of "é": NFC is the single code point U+00E9, NFD is "e" + the combining accent U+0301.
    const NFC_E: &str = "caf\u{00e9}.txt";
    const NFD_E: &str = "cafe\u{0301}.txt";

    #[test]
    fn nfd_and_nfc_spellings_fold_together() {
        // This is the heart of the divergence: without NFC the two strings are unequal,
        // so filter fails to recognize the very file compare recognizes
        assert_ne!(NFC_E, NFD_E, "premise: the two spellings really do differ byte-wise");
        assert_eq!(norm_key(NFC_E, false), norm_key(NFD_E, false));
        assert_eq!(norm_key(NFC_E, true), norm_key(NFD_E, true));
    }

    #[test]
    fn case_folding_is_opt_in() {
        assert_eq!(norm_key("A/b.TXT", false), "A/b.TXT");
        assert_eq!(norm_key("A/b.TXT", true), "A/B.TXT");
    }

    #[test]
    fn fold_matches_case_insensitive_norm_key() {
        // The CJK entry is fixture data, not prose: it covers folding a mixed
        // non-ASCII/ASCII string. Anglicising it would delete the coverage.
        for s in ["A/b.TXT", NFC_E, NFD_E, "", "混合Case"] {
            assert_eq!(fold(s), norm_key(s, true));
        }
    }

    #[test]
    fn host_becomes_filename_safe() {
        assert_eq!(safe_host("WIN01"), "WIN01");
        assert_eq!(safe_host("my.host:2222"), "my-host-2222");
        // Fixture data: the assertion is that non-ASCII host characters each collapse
        // to '-'. An ASCII input would map to itself and assert nothing.
        assert_eq!(safe_host("主机"), "--");
    }
}
