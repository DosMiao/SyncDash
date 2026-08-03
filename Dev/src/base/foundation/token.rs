//! Opaque identifiers: 128 random bits spelled as 32 lowercase hexadecimal characters.
//!
//! Lock ledger and owner ids, staged-name tokens, run-record ids and job ids are all this one
//! shape. Generation and validation live together so a reader can never accept a spelling the
//! generator cannot produce.

/// Random bytes behind a 128-bit opaque identifier.
pub const TOKEN_BYTES: usize = 16;

/// Characters in the lowercase-hex spelling of a 128-bit identifier.
pub const TOKEN_HEX_LEN: usize = TOKEN_BYTES * 2;

/// Lowercase hexadecimal, two characters per byte.
pub fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut spelled = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(spelled, "{byte:02x}").expect("writing into a String cannot fail");
    }
    spelled
}

/// 128 fresh random bits as `TOKEN_HEX_LEN` lowercase hex characters.
///
/// The caller owns the error type: an entropy source that cannot answer means something different
/// to a lock claim than it does to a staged file name.
pub fn random_token_128() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(hex_lower(&bytes))
}

/// Whether `value` is exactly `expected_len` lowercase hexadecimal characters. Uppercase is
/// refused because two spellings of one identifier would compare unequal as strings.
pub fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_token_satisfies_the_validator() {
        let token = random_token_128().unwrap();
        assert_eq!(token.len(), TOKEN_HEX_LEN);
        assert!(is_lower_hex(&token, TOKEN_HEX_LEN));
        assert_ne!(token, random_token_128().unwrap());
    }

    #[test]
    fn uppercase_and_wrong_length_are_refused() {
        assert!(is_lower_hex("00ff", 4));
        assert!(!is_lower_hex("00FF", 4));
        assert!(!is_lower_hex("00ff", 5));
        assert!(!is_lower_hex("00fg", 4));
    }
}
