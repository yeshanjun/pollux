use crate::db::{DbGeminiCliResource, GeminiCliCreate};
use crate::error::PolluxError;
use crate::providers::manifest::{GeminiCliLease, GeminiCliProfile};
use crate::providers::traits::scheduler::{CredentialId, Schedulable};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiCliResource {
    email: Option<String>,
    sub: String,
    project_id: String,
    refresh_token: String,
    access_token: String,
    expiry: DateTime<Utc>,
}

impl Default for GeminiCliResource {
    fn default() -> Self {
        Self {
            email: None,
            sub: String::new(),
            project_id: String::new(),
            refresh_token: String::new(),
            access_token: String::new(),
            expiry: Utc::now(),
        }
    }
}

impl GeminiCliResource {
    /// Return true if current time is within 5 minutes of expiry (inclusive).
    /// This early-expiry buffer avoids edge cases during requests.
    pub fn is_expired(&self) -> bool {
        Utc::now() + Duration::minutes(5) >= self.expiry
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn sub(&self) -> &str {
        &self.sub
    }

    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }

    pub fn set_project_id(&mut self, project_id: String) {
        self.project_id = project_id;
    }

    #[allow(dead_code)]
    pub fn set_sub(&mut self, sub: String) {
        self.sub = sub;
    }

    pub fn refresh_token(&self) -> &str {
        &self.refresh_token
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn expiry(&self) -> DateTime<Utc> {
        self.expiry
    }

    /// Merge updates from any JSON-serializable payload into this resource.
    /// - Accepts any `T: Serialize` and converts to `serde_json::Value` internally.
    /// - Supports both OAuth token response (`access_token`, `expires_in`)
    ///   and full credential JSON (`project_id`, `expiry`, etc.).
    /// - Only updates fields present in the JSON; others remain unchanged.
    pub fn update_credential(&mut self, payload: impl Serialize) -> Result<(), PolluxError> {
        use crate::providers::credential_update::{apply_expiry, parse_patch, set_opt, set_plain};

        #[derive(Debug, Default, Deserialize)]
        struct CredentialPatch {
            email: Option<String>,
            sub: Option<String>,
            project_id: Option<String>,
            refresh_token: Option<String>,
            access_token: Option<String>,
            expiry: Option<DateTime<Utc>>,
            expires_in: Option<i64>,
        }

        let patch: CredentialPatch = parse_patch(payload)?;

        set_opt(&mut self.email, patch.email);
        set_plain(&mut self.sub, patch.sub);
        set_plain(&mut self.project_id, patch.project_id);
        set_plain(&mut self.refresh_token, patch.refresh_token);
        set_plain(&mut self.access_token, patch.access_token);
        apply_expiry(&mut self.expiry, patch.expires_in, patch.expiry);

        debug!(
            "Project_ID {}, resource updated successfully",
            self.project_id
        );
        Ok(())
    }

    /// Build a resource from any JSON-like payload by applying updates to a default struct.
    #[cfg(any(test, feature = "bench"))]
    pub fn from_payload(payload: impl Serialize) -> Result<Self, PolluxError> {
        let mut cred = GeminiCliResource::default();
        cred.update_credential(payload)?;
        Ok(cred)
    }
}

impl Schedulable for GeminiCliResource {
    type Lease = GeminiCliLease;

    fn identifier(&self) -> &str {
        &self.project_id
    }

    fn is_expired(&self) -> bool {
        self.is_expired()
    }

    fn make_lease(&self, id: CredentialId) -> GeminiCliLease {
        GeminiCliLease {
            id,
            project_id: self.project_id.clone(),
            access_token: self.access_token.clone(),
            email: self.email.clone(),
        }
    }
}

impl From<GeminiCliProfile> for GeminiCliResource {
    fn from(profile: GeminiCliProfile) -> Self {
        GeminiCliResource {
            sub: String::new(),
            project_id: profile.project_id,
            refresh_token: profile.refresh_token,
            access_token: profile.access_token.unwrap_or_default(),
            ..Default::default()
        }
    }
}

impl From<DbGeminiCliResource> for GeminiCliResource {
    fn from(d: DbGeminiCliResource) -> Self {
        GeminiCliResource {
            email: d.email,
            sub: d.sub,
            project_id: d.project_id,
            refresh_token: d.refresh_token,
            access_token: d.access_token.unwrap_or_default(),
            expiry: d.expiry,
        }
    }
}

impl From<GeminiCliResource> for GeminiCliCreate {
    fn from(cred: GeminiCliResource) -> Self {
        GeminiCliCreate {
            email: cred.email,
            sub: cred.sub,
            project_id: cred.project_id,
            refresh_token: cred.refresh_token,
            access_token: Some(cred.access_token),
            expiry: cred.expiry,
        }
    }
}
