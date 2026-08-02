//! Private root-binding derivation for persisted state generations.

pub(super) fn digest(binding: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(binding).to_hex())
}

/// Domain-separate canonical logical identities from historical raw identity bytes.
pub(crate) fn logical_binding(key: &str) -> Vec<u8> {
    let mut binding = b"syncdash.logical-state.v2\0".to_vec();
    binding.extend_from_slice(key.as_bytes());
    binding
}
