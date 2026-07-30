//! Input normalisation and validation.
//!
//! Small enough to unit test without a database, and worth doing: the rules
//! here are exactly the ones that were wrong in the original Go and are now
//! pinned by the e2e suite.

/// Enforced at registration. Existing shorter passwords keep working.
pub const MIN_PASSWORD_LENGTH: usize = 8;

/// Bounds a deliberately unauthenticated endpoint.
pub const MAX_FEEDBACK_LENGTH: usize = 4096;

/// Canonicalises an address for storage and lookup.
///
/// Folds **ASCII only**. Go originally used `strings.ToLower`, a per-rune
/// Unicode mapping that is not a round trip: U+FB00 ("ff") uppercases to the
/// two-character "FF", which lowercases to "ff", so an address containing it
/// registered in uppercase could never be logged into again.
///
/// ASCII-only folding is round-trip safe by construction and is identical in
/// both implementations, which matters because the two share a database.
pub fn sanitize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

#[derive(Debug, PartialEq, Eq)]
pub enum CredentialError {
    InvalidEmail,
    PasswordTooShort,
}

impl CredentialError {
    pub fn message(&self) -> String {
        match self {
            Self::InvalidEmail => "invalid email address".to_owned(),
            Self::PasswordTooShort => {
                format!("password must be at least {MIN_PASSWORD_LENGTH} characters")
            }
        }
    }
}

/// Validates a registration payload.
pub fn validate_credentials(email: &str, password: &str) -> Result<(), CredentialError> {
    if !is_plausible_email(email) {
        return Err(CredentialError::InvalidEmail);
    }

    if password.len() < MIN_PASSWORD_LENGTH {
        return Err(CredentialError::PasswordTooShort);
    }

    Ok(())
}

/// Mirrors what Go's `net/mail.ParseAddress` accepts for our purposes: a
/// non-empty local part, a single `@`, and a domain containing a dot.
///
/// Deliberately conservative rather than RFC-complete -- the goal is to reject
/// obvious non-addresses, not to adjudicate exotic ones.
fn is_plausible_email(email: &str) -> bool {
    if email.contains(char::is_whitespace) {
        return false;
    }

    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };

    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return false;
    }

    // A bare hostname is not routable mail in practice.
    match domain.split_once('.') {
        Some((host, tld)) => !host.is_empty() && !tld.is_empty() && !tld.starts_with('.'),
        None => false,
    }
}

/// Rejects timezone names that `chrono_tz` might not resolve identically to Go.
///
/// Two cases matter. Go's `time.LoadLocation("")` resolves to UTC, so a blank
/// field used to be accepted and stored verbatim, leaving the user on a
/// timezone of "" that nothing can resolve later. And redundant separators such
/// as "Europe//Amsterdam" resolve in Go because `LoadLocation` ends up opening
/// a filesystem path the OS normalises -- chrono-tz rejects those outright, so
/// screening them here keeps the two implementations in agreement.
pub fn is_canonical_timezone(timezone: &str) -> bool {
    if timezone.trim().is_empty() {
        return false;
    }

    // Rejects leading, trailing and doubled separators in one pass.
    timezone.split('/').all(|segment| !segment.is_empty())
}

/// Whether the name resolves to a real IANA zone.
pub fn timezone_exists(timezone: &str) -> bool {
    timezone.parse::<chrono_tz::Tz>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_case_is_folded_and_whitespace_trimmed() {
        assert_eq!(
            sanitize_email("  MiXeD@Example.TEST  "),
            "mixed@example.test"
        );
    }

    #[test]
    fn non_ascii_is_preserved_verbatim() {
        // The bug this replaced: Go's ToLower mapped these in ways that were
        // not reversible, making such accounts permanently unreachable.
        for address in ["ﬀ-x@example.test", "ß@example.test", "İ@example.test"] {
            assert_eq!(sanitize_email(address), address);
        }
    }

    #[test]
    fn sanitising_is_idempotent() {
        for address in ["  A@B.test ", "ﬀ@x.test", "already@lower.test"] {
            let once = sanitize_email(address);
            assert_eq!(sanitize_email(&once), once);
        }
    }

    #[test]
    fn rejects_non_addresses() {
        for address in [
            "definitely not an email",
            "no-at-sign.example.test",
            "@example.test",
            "",
            "user@",
            "user@nodot",
            "a@b@c.test",
        ] {
            assert_eq!(
                validate_credentials(address, "longenoughpassword"),
                Err(CredentialError::InvalidEmail),
                "{address:?} should have been rejected"
            );
        }
    }

    #[test]
    fn accepts_ordinary_addresses() {
        for address in [
            "user@example.test",
            "a.b+c@sub.example.co.uk",
            "ﬀ@example.test",
        ] {
            assert!(
                validate_credentials(address, "longenoughpassword").is_ok(),
                "{address:?} should have been accepted"
            );
        }
    }

    #[test]
    fn enforces_the_password_floor() {
        assert_eq!(
            validate_credentials("a@b.test", "1234567"),
            Err(CredentialError::PasswordTooShort)
        );
        assert!(validate_credentials("a@b.test", "12345678").is_ok());
    }

    #[test]
    fn rejects_non_canonical_timezones() {
        for zone in [
            "",
            "   ",
            "Europe//Amsterdam",
            "/Europe/Amsterdam",
            "Europe/Amsterdam/",
        ] {
            assert!(!is_canonical_timezone(zone), "{zone:?} should be rejected");
        }
    }

    #[test]
    fn accepts_real_zones() {
        for zone in ["UTC", "Europe/Amsterdam", "America/Los_Angeles"] {
            assert!(is_canonical_timezone(zone));
            assert!(timezone_exists(zone));
        }
    }

    #[test]
    fn unknown_zones_do_not_resolve() {
        assert!(!timezone_exists("Mars/Olympus_Mons"));
        assert!(!timezone_exists("Not/AZone"));
    }
}
