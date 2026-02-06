use std::fmt;

/// Untrusted input: a refresh token seed submitted from external sources.
///
/// This must never be deserialized directly from external payloads via a wider struct.
#[derive(Clone)]
pub(crate) struct CodexRefreshTokenSeed {
    refresh_token: String,
}

impl CodexRefreshTokenSeed {
    pub(crate) fn new(refresh_token: String) -> Option<Self> {
        let refresh_token = refresh_token.trim().to_string();
        if refresh_token.is_empty() {
            return None;
        }
        Some(Self { refresh_token })
    }

    pub(crate) fn refresh_token(&self) -> &str {
        &self.refresh_token
    }
}

impl fmt::Debug for CodexRefreshTokenSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodexRefreshTokenSeed")
            .field("refresh_token", &"<redacted>")
            .finish()
    }
}
