use crate::db::{DbGeminiCliResource, GeminiCliCreate};
use crate::error::PolluxError;
use crate::providers::manifest::GeminiCliProfile;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GeminiCliResource {
    email: Option<String>,
    sub: String,
    project_id: String,
    refresh_token: String,
    access_token: Option<String>,
    expiry: DateTime<Utc>,
}

impl Default for GeminiCliResource {
    fn default() -> Self {
        Self {
            email: None,
            sub: String::new(),
            project_id: String::new(),
            refresh_token: String::new(),
            access_token: None,
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

    #[allow(dead_code)]
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

    pub fn access_token(&self) -> Option<&str> {
        self.access_token.as_deref()
    }

    #[allow(dead_code)]
    pub fn expiry(&self) -> DateTime<Utc> {
        self.expiry
    }

    /// Merge updates from any JSON-serializable payload into this resource.
    /// - Accepts any `T: Serialize` and converts to `serde_json::Value` internally.
    /// - Supports both OAuth token response (access_token, expires_in)
    ///   and full credential JSON (project_id, expiry, etc.).
    /// - Only updates fields present in the JSON; others remain unchanged.
    pub fn update_credential(&mut self, payload: impl Serialize) -> Result<(), PolluxError> {
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
        set_plain!(sub);
        set_plain!(project_id);
        set_plain!(refresh_token);
        set_opt!(access_token);

        if let Some(secs) = patch.expires_in {
            self.expiry = Utc::now() + Duration::seconds(secs);
        } else if let Some(dt) = patch.expiry {
            self.expiry = dt;
        }

        debug!(
            "Project_ID {}, resource updated successfully",
            self.project_id
        );
        Ok(())
    }

    /// Build a resource from any JSON-like payload by applying updates to a default struct.
    #[cfg(test)]
    pub fn from_payload(payload: impl Serialize) -> Result<Self, PolluxError> {
        let mut cred = GeminiCliResource::default();
        cred.update_credential(payload)?;
        Ok(cred)
    }
}

impl From<GeminiCliProfile> for GeminiCliResource {
    fn from(profile: GeminiCliProfile) -> Self {
        GeminiCliResource {
            sub: String::new(),
            project_id: profile.project_id,
            refresh_token: profile.refresh_token,
            access_token: profile.access_token,
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
            access_token: d.access_token,
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
            access_token: cred.access_token,
            expiry: cred.expiry,
        }
    }
}
