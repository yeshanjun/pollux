use crate::db::{CodexCreate, DbCodexResource};
use crate::error::PolluxError;
use crate::providers::codex::CodexRefreshTokenSeed;
use crate::providers::codex::oauth::OauthTokenResponse;
use crate::providers::manifest::{CodexLease, CodexProfile};
use chrono::{DateTime, Duration, Utc};
use oauth2::TokenResponse;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// In-memory credential state for the Codex provider.
///
/// This mirrors `geminicli::resource` so that token refresh / hot reload can
/// evolve in the same direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CodexResource {
    email: Option<String>,
    account_id: String,
    sub: String,
    refresh_token: String,
    access_token: String,
    expiry: DateTime<Utc>,
    chatgpt_plan_type: Option<String>,
}

impl Default for CodexResource {
    fn default() -> Self {
        Self {
            email: None,
            account_id: String::new(),
            sub: String::new(),
            refresh_token: String::new(),
            access_token: String::new(),
            expiry: Utc::now(),
            chatgpt_plan_type: None,
        }
    }
}

impl CodexResource {
    /// Return true if current time is within 5 minutes of expiry (inclusive).
    /// This early-expiry buffer avoids edge cases during requests.
    #[allow(dead_code)]
    pub fn is_expired(&self) -> bool {
        Utc::now() + Duration::minutes(5) >= self.expiry
    }

    #[allow(dead_code)]
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    #[allow(dead_code)]
    pub fn sub(&self) -> &str {
        &self.sub
    }

    #[allow(dead_code)]
    pub fn refresh_token(&self) -> &str {
        &self.refresh_token
    }

    #[allow(dead_code)]
    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    #[allow(dead_code)]
    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }

    #[allow(dead_code)]
    pub fn chatgpt_plan_type(&self) -> Option<&str> {
        self.chatgpt_plan_type.as_deref()
    }

    #[allow(dead_code)]
    pub fn expiry(&self) -> DateTime<Utc> {
        self.expiry
    }

    #[allow(dead_code)]
    pub fn into_lease(self, id: u64) -> CodexLease {
        CodexLease {
            id,
            access_token: self.access_token,
            account_id: self.account_id,
        }
    }

    /// Merge updates from any JSON-serializable payload into this resource.
    ///
    /// This accepts both:
    /// - full credential JSON (account_id, refresh_token, expiry, etc.)
    /// - OAuth token refresh payloads (access_token, expires_in)
    ///
    /// Only updates fields present in the JSON; others remain unchanged.
    #[allow(dead_code)]
    pub fn update_credential(&mut self, payload: impl Serialize) -> Result<(), PolluxError> {
        #[derive(Debug, Default, Deserialize)]
        struct CredentialPatch {
            email: Option<String>,
            account_id: Option<String>,
            sub: Option<String>,
            refresh_token: Option<String>,
            access_token: Option<String>,
            expiry: Option<DateTime<Utc>>,
            expires_in: Option<i64>,
            chatgpt_plan_type: Option<String>,
        }

        let value = serde_json::to_value(payload)?;
        let patch: CredentialPatch = serde_json::from_value(value)?;

        macro_rules! set_plain {
            ($field:ident) => {
                if let Some(v) = patch.$field {
                    self.$field = v;
                }
            };
        }
        macro_rules! set_opt {
            ($field:ident) => {
                if let Some(v) = patch.$field {
                    self.$field = Some(v);
                }
            };
        }

        set_opt!(email);
        set_plain!(account_id);
        set_plain!(sub);
        set_plain!(refresh_token);
        set_plain!(access_token);
        set_opt!(chatgpt_plan_type);

        if let Some(secs) = patch.expires_in {
            self.expiry = Utc::now() + Duration::seconds(secs);
        } else if let Some(dt) = patch.expiry {
            self.expiry = dt;
        }

        debug!(
            "account_id={}, sub={}, Codex resource updated successfully",
            self.account_id, self.sub
        );

        Ok(())
    }

    /// Build a resource from any JSON-like payload by applying updates to a default struct.
    #[cfg(test)]
    pub fn from_payload(payload: impl Serialize) -> Result<Self, PolluxError> {
        let mut cred = CodexResource::default();
        cred.update_credential(payload)?;
        Ok(cred)
    }

    pub(super) fn try_from_oauth_token_response(
        token_response: OauthTokenResponse,
        refresh_seed: Option<CodexRefreshTokenSeed>,
    ) -> Result<Self, PolluxError> {
        let access_token = token_response.access_token().secret().trim().to_string();
        if access_token.is_empty() {
            return Err(PolluxError::UnexpectedError(
                "Missing access_token in OAuth token response".to_string(),
            ));
        }

        let expires_in = token_response
            .expires_in()
            .unwrap_or_else(|| std::time::Duration::from_secs(60 * 60));
        let expiry = Utc::now() + Duration::seconds(expires_in.as_secs() as i64);

        let refresh_token = token_response
            .refresh_token()
            .map(|t| t.secret().trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| refresh_seed.as_ref().map(|s| s.refresh_token().to_string()))
            .ok_or_else(|| {
                PolluxError::UnexpectedError(
                    "Missing refresh_token in OAuth token response".to_string(),
                )
            })?;

        let id_token = token_response
            .extra_fields()
            .id_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                PolluxError::UnexpectedError("Missing id_token in OAuth token response".to_string())
            })?;

        let identity = super::identity::identity_from_id_token(id_token)?;

        Ok(CodexResource {
            email: identity.email,
            account_id: identity.account_id,
            sub: identity.sub,
            refresh_token,
            access_token,
            expiry,
            chatgpt_plan_type: identity.chatgpt_plan_type,
        })
    }
}

impl TryFrom<CodexProfile> for CodexResource {
    type Error = PolluxError;

    fn try_from(profile: CodexProfile) -> Result<Self, Self::Error> {
        let access_token = profile
            .access_token
            .ok_or(PolluxError::MissingAccessToken)?;
        let expiry = profile.expiry.ok_or(PolluxError::MissingExpiry)?;

        Ok(CodexResource {
            email: profile.email,
            account_id: profile.account_id,
            sub: profile.sub,
            refresh_token: profile.refresh_token,
            access_token,
            expiry,
            chatgpt_plan_type: profile.chatgpt_plan_type,
        })
    }
}

impl From<DbCodexResource> for CodexResource {
    fn from(d: DbCodexResource) -> Self {
        CodexResource {
            email: d.email,
            account_id: d.account_id,
            sub: d.sub,
            refresh_token: d.refresh_token,
            access_token: d.access_token,
            expiry: d.expiry,
            chatgpt_plan_type: d.chatgpt_plan_type,
        }
    }
}

impl From<CodexResource> for CodexCreate {
    fn from(cred: CodexResource) -> Self {
        CodexCreate {
            email: cred.email,
            account_id: cred.account_id,
            sub: cred.sub,
            refresh_token: cred.refresh_token,
            access_token: cred.access_token,
            expiry: cred.expiry,
            chatgpt_plan_type: cred.chatgpt_plan_type,
        }
    }
}

impl From<&CodexResource> for CodexCreate {
    fn from(cred: &CodexResource) -> Self {
        CodexCreate {
            email: cred.email.clone(),
            account_id: cred.account_id.clone(),
            sub: cred.sub.clone(),
            refresh_token: cred.refresh_token.clone(),
            access_token: cred.access_token.clone(),
            expiry: cred.expiry,
            chatgpt_plan_type: cred.chatgpt_plan_type.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn is_expired_true_when_within_buffer() {
        let cred = CodexResource::from_payload(json!({
            "account_id": "acct_test",
            "refresh_token": "rt",
            "access_token": "at",
            "expiry": Utc::now() + chrono::Duration::minutes(4),
        }))
        .expect("valid payload");

        assert!(cred.is_expired());
    }

    #[test]
    fn update_credential_supports_expires_in() {
        let mut cred = CodexResource::from_payload(json!({
            "account_id": "acct_test",
            "refresh_token": "rt",
            "access_token": "at0",
            "expiry": Utc::now() - chrono::Duration::minutes(10),
        }))
        .expect("valid payload");

        cred.update_credential(json!({
            "access_token": "at1",
            "expires_in": 3600,
        }))
        .expect("valid update");

        assert_eq!(cred.access_token(), "at1");
        assert!(!cred.is_expired());
    }
}
