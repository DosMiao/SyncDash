//! Cryptographically secure opaque identifiers for desktop authorities and immutable evidence.

pub(crate) fn random_hex<const BYTE_COUNT: usize>(failure_context: &str) -> Result<String, String> {
    let mut bytes = [0_u8; BYTE_COUNT];
    getrandom::fill(&mut bytes).map_err(|error| format!("{failure_context}: {error}"))?;
    let mut token = String::with_capacity(BYTE_COUNT * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        token.push(HEX[(byte >> 4) as usize] as char);
        token.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_have_the_requested_entropy_width_and_hex_shape() {
        let first = random_hex::<16>("test token").unwrap();
        let second = random_hex::<16>("test token").unwrap();
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }
}
