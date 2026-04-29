//! Untrusted refresh-token input shared across providers.
//!
//! A `RefreshTokenSeed` is the minimal payload needed to start an OAuth refresh
//! cycle for a credential that has not yet been onboarded. It deliberately does
//! not derive `Debug`: refresh tokens are long-lived secrets and any accidental
//! `{:?}` formatting (tracing events, panic messages, error chains) must not
//! leak them.

use std::fmt;

/// Untrusted input: a refresh token submitted from an external source.
///
/// Construct via [`RefreshTokenSeed::new`], which trims whitespace and rejects
/// empty values. The contained token is never exposed through `Debug`.
#[derive(Clone)]
pub(crate) struct RefreshTokenSeed {
    refresh_token: String,
}

impl RefreshTokenSeed {
    /// Build a seed from a raw token string. Returns `None` when the input is
    /// empty after trimming.
    pub(crate) fn new(refresh_token: &str) -> Option<Self> {
        let refresh_token = refresh_token.trim().to_string();
        if refresh_token.is_empty() {
            return None;
        }
        Some(Self { refresh_token })
    }

    /// Borrow the underlying token. Callers are responsible for not logging
    /// or otherwise leaking the returned value.
    pub(crate) fn refresh_token(&self) -> &str {
        &self.refresh_token
    }
}

impl fmt::Debug for RefreshTokenSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshTokenSeed")
            .field("refresh_token", &"<redacted>")
            .finish()
    }
}
