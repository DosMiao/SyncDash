//! Cryptographically secure opaque identifiers for desktop authorities and immutable evidence.

/// The engine owns the hex spelling so a desktop identifier and an engine token are the same
/// shape; only the entropy width and the failure message belong to this side.
pub(crate) fn random_hex<const BYTE_COUNT: usize>(failure_context: &str) -> Result<String, String> {
    let mut bytes = [0_u8; BYTE_COUNT];
    getrandom::fill(&mut bytes).map_err(|error| format!("{failure_context}: {error}"))?;
    Ok(syncdash::foundation::token::hex_lower(&bytes))
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
