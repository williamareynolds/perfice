//! Password hashing.
//!
//! There is no existing database to stay compatible with, so this does not
//! reproduce Go's cost parameters. Go used RFC 9106's *second* recommendation
//! (64 MiB, t=3, p=4), which is a fine choice but roughly five times the work
//! of the current OWASP recommendation for interactive logins -- enough to be
//! the dominant cost in a login-heavy test suite.
//!
//! These are the `argon2` crate's defaults, stated explicitly so that a future
//! change to the crate's idea of "default" is a deliberate decision here rather
//! than a silent change to every stored hash.
//!
//! Output is a standard PHC string, so the parameters live alongside the hash
//! and can be raised later without invalidating existing rows: verification
//! reads them from the string rather than from this module.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};

/// OWASP's argon2id recommendation for interactive use: 19 MiB, 2 passes.
const MEMORY_COST_KIB: u32 = 19456;
const TIME_COST: u32 = 2;
const PARALLELISM: u32 = 1;
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

    /// A hash produced with different cost parameters -- here Go's old
    /// settings. Nothing in the deployment produces these any more, but
    /// verification must stay parameter-agnostic so the costs above can be
    /// raised later without locking anyone out.
    const FOREIGN_PARAMS_HASH: &str = "$argon2id$v=19$m=65536,t=3,p=4$2g558KZUS9EaJKB94MALrw$z4HqgRT9fvc7tS/maH4z5rOGABZnL+qk5Jn2FKQAwfE";
    const FOREIGN_PARAMS_PASSWORD: &str = "correct-horse-battery-staple";

    #[test]
    fn verifies_hashes_written_with_other_cost_parameters() {
        assert!(
            verify(FOREIGN_PARAMS_PASSWORD, FOREIGN_PARAMS_HASH),
            "verification must read parameters from the PHC string"
        );
    }

    #[test]
    fn rejects_the_wrong_password() {
        assert!(!verify("wrong", FOREIGN_PARAMS_HASH));
    }

    #[test]
    fn round_trips_its_own_hashes() {
        let encoded = hash("hunter22").unwrap();
        assert!(verify("hunter22", &encoded));
        assert!(!verify("hunter2", &encoded));
    }

    #[test]
    fn emits_the_configured_parameters() {
        let encoded = hash("whatever").unwrap();
        assert!(
            encoded.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "unexpected parameters: {encoded}"
        );
    }

    #[test]
    fn a_malformed_stored_hash_is_not_an_error() {
        assert!(!verify("anything", "not-a-phc-string"));
        assert!(!verify("anything", ""));
    }
}
