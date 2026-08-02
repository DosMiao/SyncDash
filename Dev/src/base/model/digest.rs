//! L0 content identity: the BLAKE3 digest, and whether a recorded one can be verified against.
//!
//! Two separate questions live here, and conflating them is a data-safety bug in either direction.
//!
//! *Is this string a digest at all* — 64 lowercase hex characters. Four modules had each written
//! that check out by hand (plan validation, pack format validation, version-restore manifests, and
//! the desktop's evidence artifacts), so a plan, a package, a retained version, and a Compare
//! result each decided independently what counted as content identity.
//!
//! *Is this digest a promise about the whole file* — a `~` prefix marks a **sampled** digest, taken
//! from a few windows rather than the full stream. A sampled digest is enough to notice a change
//! and never enough to verify one: comparing it to a freshly computed full hash always disagrees,
//! so treating it as verifiable would fail every copy, and treating a full one as sampled would
//! skip the verification the user asked for. That distinction was spelled `starts_with('~')` at
//! eight execution sites.
//!
//! Deliberately **not** here: the 32-character Compare `result_id`. It is a random 128-bit opaque
//! token that happens to also be lowercase hex, and it identifies evidence rather than content.
//! Folding it into this vocabulary would let a digest and an identity be used interchangeably.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

/// The number of hex characters in a BLAKE3-256 digest.
const BLAKE3_HEX_LENGTH: usize = 64;

/// Whether `value` is exactly a BLAKE3 digest: 64 lowercase hexadecimal characters, nothing else.
///
/// Uppercase is rejected rather than folded. These strings are compared for equality all over the
/// pipeline, and accepting both cases would make two spellings of the same content unequal.
pub fn is_blake3_hex(value: &str) -> bool {
    value.len() == BLAKE3_HEX_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The marker a plan puts in front of a digest taken from samples rather than the whole file.
pub const SAMPLED_PREFIX: char = '~';

/// Whether a recorded plan hash is sampled, and therefore **not** something to verify a copy
/// against.
pub fn is_sampled_digest(recorded: &str) -> bool {
    recorded.starts_with(SAMPLED_PREFIX)
}

/// The digest a copy may be verified against, or `None` when the recorded hash is sampled.
///
/// Callers that verify written content must go through this rather than testing the sigil inline:
/// the whole point is that a sampled digest has no verifiable answer.
pub fn verifiable_digest(recorded: &str) -> Option<&str> {
    (!is_sampled_digest(recorded)).then_some(recorded)
}

/// The digest body with any sampled marker removed.
pub fn digest_body(recorded: &str) -> &str {
    recorded.strip_prefix(SAMPLED_PREFIX).unwrap_or(recorded)
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct Blake3Digest(String);

impl Blake3Digest {
    pub fn parse(value: impl Into<String>) -> Result<Self, DigestError> {
        let value = value.into();
        if !is_blake3_hex(&value) {
            return Err(DigestError(value));
        }
        Ok(Self(value))
    }

    pub fn hash_bytes(bytes: &[u8]) -> Self {
        Self(blake3::hash(bytes).to_hex().to_string())
    }

    pub fn from_hash(hash: blake3::Hash) -> Self {
        Self(hash.to_hex().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for Blake3Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Blake3Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for Blake3Digest {
    type Error = DigestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for Blake3Digest {
    type Error = DigestError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigestError(String);

impl fmt::Display for DigestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid BLAKE3 digest {:?}: expected exactly {BLAKE3_HEX_LENGTH} lowercase hexadecimal characters",
            self.0
        )
    }
}

impl std::error::Error for DigestError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sixty_four_lowercase_hex_characters_are_a_digest() {
        let valid = "a".repeat(64);
        assert!(is_blake3_hex(&valid));
        assert!(Blake3Digest::parse(valid).is_ok());

        assert!(!is_blake3_hex(&"a".repeat(63)), "too short");
        assert!(!is_blake3_hex(&"a".repeat(65)), "too long");
        assert!(
            !is_blake3_hex(&"A".repeat(64)),
            "uppercase is a different string"
        );
        assert!(!is_blake3_hex(&"g".repeat(64)), "not hexadecimal");
        assert!(!is_blake3_hex(""), "empty");

        // The 32-character Compare result_id is not a digest and must not read as one.
        assert!(!is_blake3_hex(&"a".repeat(32)));
    }

    #[test]
    fn a_sampled_digest_is_never_verifiable() {
        let full = "b".repeat(64);
        let sampled = format!("~{full}");

        assert!(!is_sampled_digest(&full));
        assert_eq!(verifiable_digest(&full), Some(full.as_str()));
        assert_eq!(digest_body(&full), full);

        assert!(is_sampled_digest(&sampled));
        assert_eq!(
            verifiable_digest(&sampled),
            None,
            "a sampled digest describes windows, not the whole stream; verifying against it would \
             fail every copy"
        );
        assert_eq!(digest_body(&sampled), full);
    }
}
