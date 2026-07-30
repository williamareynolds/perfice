//! Cryptographically secure random values.
//!
//! Both the Go services that generate randomness (sync's per-user salt,
//! integration's webhook tokens, auth's refresh tokens) use crypto/rand, so
//! everything here goes through the OS generator rather than a seeded one.

use rand::RngExt;

/// The alphabet Go's `createRefreshToken` and `GenerateAlphanumericString` use.
const ALPHANUMERIC: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Returns `len` random bytes.
pub fn bytes(len: usize) -> Vec<u8> {
    let mut buffer = vec![0u8; len];
    rand::rng().fill(buffer.as_mut_slice());
    buffer
}

/// Returns a random alphanumeric string of exactly `len` characters.
///
/// Length and alphabet are part of the contract: the e2e suite asserts refresh
/// tokens are 16 alphanumeric characters and webhook tokens are 32.
pub fn alphanumeric(len: usize) -> String {
    let mut rng = rand::rng();
    (0..len)
        .map(|_| {
            let index = rng.random_range(0..ALPHANUMERIC.len());
            ALPHANUMERIC[index] as char
        })
        .collect()
}

/// Returns a value in `0..bound`, or zero when the bound is zero.
pub fn below(bound: u64) -> u64 {
    if bound == 0 {
        return 0;
    }

    rand::rng().random_range(0..bound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_stays_inside_the_bound() {
        assert_eq!(below(0), 0);
        assert_eq!(below(1), 0);
        assert!((0..10).all(|_| below(10) < 10));
    }

    #[test]
    fn bytes_have_the_requested_length() {
        assert_eq!(bytes(32).len(), 32);
        assert_eq!(bytes(0).len(), 0);
    }

    #[test]
    fn bytes_are_not_constant() {
        assert_ne!(bytes(32), bytes(32));
    }

    #[test]
    fn alphanumeric_matches_the_go_alphabet_and_length() {
        let token = alphanumeric(16);
        assert_eq!(token.chars().count(), 16);
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
