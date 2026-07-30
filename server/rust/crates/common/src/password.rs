//! Password hashing, wire-compatible with the Go implementation.
//!
//! Existing accounts were hashed by `github.com/matthewhartstonge/argon2`'s
//! `DefaultConfig`, so both directions have to keep working: Rust must verify
//! Go-produced hashes (proven by the tests below), and hashes Rust produces
//! must stay readable by Go in case of a rollback.
//!
//! Verification reads its parameters from the PHC string, so it is
//! parameter-agnostic. Hashing is not, which is why [`hasher`] pins Go's
//! settings explicitly instead of using `Argon2::default()` -- the Rust crate's
//! defaults are m=19456, t=2, p=1 and would silently produce hashes with
//! different cost parameters.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};

/// Go's `argon2.DefaultConfig()` == `MemoryConstrainedDefaults()`, i.e. the
/// second RFC 9106 recommendation: argon2id, t=3, p=4, m=2^16 KiB (64 MiB),
/// 128-bit salt, 256-bit tag.
const MEMORY_COST_KIB: u32 = 65536;
const TIME_COST: u32 = 3;
const PARALLELISM: u32 = 4;
const OUTPUT_LEN: usize = 32;

fn hasher() -> Argon2<'static> {
    let params = Params::new(MEMORY_COST_KIB, TIME_COST, PARALLELISM, Some(OUTPUT_LEN))
        .expect("argon2 parameters are valid by construction");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Hashes a password into a PHC string.
pub fn hash(password: &str) -> anyhow::Result<String> {
    // OsRng comes from the rand_core that `password_hash` itself depends on;
    // using the workspace `rand` here would be a version mismatch.
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);

    hasher()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|err| anyhow::anyhow!("failed to hash password: {err}"))
}

/// Verifies a password against a stored PHC string.
///
/// A malformed stored hash is reported as "does not match" rather than an
/// error: it must not be distinguishable from a wrong password.
pub fn verify(password: &str, encoded: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(encoded) else {
        return false;
    };

    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Produced by Go's `argon2.DefaultConfig().HashEncoded(...)`.
    const GO_HASH: &str = "$argon2id$v=19$m=65536,t=3,p=4$2g558KZUS9EaJKB94MALrw$z4HqgRT9fvc7tS/maH4z5rOGABZnL+qk5Jn2FKQAwfE";
    const GO_PASSWORD: &str = "correct-horse-battery-staple";

    #[test]
    fn verifies_a_hash_produced_by_the_go_implementation() {
        assert!(
            verify(GO_PASSWORD, GO_HASH),
            "existing Go-hashed passwords must keep working"
        );
    }

    #[test]
    fn rejects_the_wrong_password() {
        assert!(!verify("wrong", GO_HASH));
    }

    #[test]
    fn round_trips_its_own_hashes() {
        let encoded = hash("hunter22").unwrap();
        assert!(verify("hunter22", &encoded));
        assert!(!verify("hunter2", &encoded));
    }

    #[test]
    fn emits_the_same_parameters_go_would() {
        // A rollback to Go must still be able to read what Rust wrote.
        let encoded = hash("whatever").unwrap();
        assert!(
            encoded.starts_with("$argon2id$v=19$m=65536,t=3,p=4$"),
            "unexpected parameters: {encoded}"
        );
    }

    #[test]
    fn a_malformed_stored_hash_is_not_an_error() {
        assert!(!verify("anything", "not-a-phc-string"));
        assert!(!verify("anything", ""));
    }
}
