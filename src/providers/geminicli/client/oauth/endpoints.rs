use super::types::UserTier;
use crate::error::{OauthError, PolluxError};
use crate::providers::geminicli::{
    GEMINICLI_SCOPES, GOOGLE_AUTH_URL, GOOGLE_TOKEN_URI, LOAD_CODE_ASSIST_URL, OAUTH_CALLBACK_URL,
    ONBOARD_CODE_ASSIST_URL,
};
use oauth2::{
    AuthUrl, AuthorizationCode, Client as OAuth2Client, ClientId, ClientSecret, CsrfToken,
    EndpointNotSet, EndpointSet, ExtraTokenFields, PkceCodeChallenge, PkceCodeVerifier,
    RefreshToken, StandardRevocableToken, StandardTokenResponse, TokenUrl,
    basic::{
        BasicErrorResponse, BasicRevocationErrorResponse, BasicTokenIntrospectionResponse,
        BasicTokenType,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::LazyLock;
use tracing::info;

/// Stateless Google OAuth Endpoints.
pub(crate) struct GoogleOauthEndpoints;

/// Fixed Gemini CLI OAuth client credentials (not overridable via config).
const GCLI_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const GCLI_CLIENT_SECRET: &str = "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";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OnboardMetadata {
    #[serde(default)]
    ide_type: String,
    platform: String,
    plugin_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    duet_project: Option<String>,
}

impl Default for OnboardMetadata {
    fn default() -> Self {
        Self {
            ide_type: "IDE_UNSPECIFIED".to_string(),
            platform: "PLATFORM_UNSPECIFIED".to_string(),
            plugin_type: "GEMINI".to_string(),
            duet_project: None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OnboardRequest {
    tier_id: UserTier,
    #[serde(skip_serializing_if = "Option::is_none")]
    cloudaicompanion_project: Option<String>,
    #[serde(default)]
    metadata: OnboardMetadata,
}

pub(crate) static OAUTH_CLIENT: LazyLock<GoogleOauth2Client> =
    LazyLock::new(|| build_oauth2_client().expect("valid Google OAuth2 client with redirect"));

impl GoogleOauthEndpoints {
    /// Return the shared Google OAuth2 client with redirect configured.
    pub(crate) fn client() -> &'static GoogleOauth2Client {
        &OAUTH_CLIENT
    }

    /// Build an auth URL with default scopes and PKCE challenge preset.
    pub(crate) fn build_authorize_url(pkce_challenge: PkceCodeChallenge) -> (url::Url, CsrfToken) {
        let mut req = Self::client()
            .authorize_url(CsrfToken::new_random)
            .set_pkce_challenge(pkce_challenge)
            .add_extra_param("access_type", "offline")
            .add_extra_param("prompt", "consent");

        for scope in GEMINICLI_SCOPES.iter() {
            req = req.add_scope(scope.clone());
        }

        req.url()
    }

    /// Refresh the access token using the current refresh token.
    pub(crate) async fn refresh_access_token(
        refresh_token: &str,
        http_client: reqwest::Client,
    ) -> Result<GoogleTokenResponse, OauthError> {
        let token_result: GoogleTokenResponse = Self::client()
            .exchange_refresh_token(&RefreshToken::new(refresh_token.to_string()))
            .request_async(&http_client)
            .await?;
        Ok(token_result)
    }

    /// Exchange an authorization code (PKCE) for tokens.
    pub(crate) async fn exchange_authorization_code(
        code: AuthorizationCode,
        verifier: PkceCodeVerifier,
        http_client: reqwest::Client,
    ) -> Result<GoogleTokenResponse, OauthError> {
        let token_result: GoogleTokenResponse = Self::client()
            .exchange_code(code)
            .set_pkce_verifier(verifier)
            .request_async(&http_client)
            .await?;
        info!("OAuth2 code exchange completed successfully");
        Ok(token_result)
    }

    /// Call Cloud Code's loadCodeAssist to fetch subscription metadata and the companion project.
    pub(crate) async fn load_code_assist(
        access_token: impl AsRef<str>,
        http_client: reqwest::Client,
    ) -> Result<Value, OauthError> {
        let resp = http_client
            .post(LOAD_CODE_ASSIST_URL)
            .bearer_auth(access_token.as_ref())
            .json(&json!({}))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(OauthError::UpstreamStatus(resp.status()));
        }

        let body: Value = resp.json().await?;
        Ok(body)
    }

    /// Call Cloud Code's onboardUser to provision a companion project and tier.
    pub(crate) async fn onboard_user(
        access_token: impl AsRef<str>,
        tier: UserTier,
        cloudaicompanion_project: Option<String>,
        http_client: reqwest::Client,
    ) -> Result<Value, OauthError> {
        let request = OnboardRequest {
            tier_id: tier,
            cloudaicompanion_project: cloudaicompanion_project.clone(),
            metadata: OnboardMetadata {
                duet_project: cloudaicompanion_project,
                ..Default::default()
            },
        };

        let resp = http_client
            .post(ONBOARD_CODE_ASSIST_URL)
            .bearer_auth(access_token.as_ref())
            .json(&request)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(OauthError::UpstreamStatus(resp.status()));
        }

        let body: Value = resp.json().await?;
        Ok(body)
    }
}

/// Build the Google OAuth2 client.
fn build_oauth2_client() -> Result<GoogleOauth2Client, PolluxError> {
    let client = OAuth2Client::new(ClientId::new(GCLI_CLIENT_ID.to_string()))
        .set_client_secret(ClientSecret::new(GCLI_CLIENT_SECRET.to_string()))
        .set_auth_uri(AuthUrl::new(GOOGLE_AUTH_URL.to_string())?)
        .set_token_uri(TokenUrl::new(GOOGLE_TOKEN_URI.to_string())?)
        .set_redirect_uri(OAUTH_CALLBACK_URL.clone());
    Ok(client)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct GoogleTokenField {
    #[serde(rename = "id_token")]
    pub id_token: Option<String>,
}
impl ExtraTokenFields for GoogleTokenField {}

pub(crate) type GoogleTokenResponse = StandardTokenResponse<GoogleTokenField, BasicTokenType>;

pub(crate) type GoogleOauth2Client<
    HasAuthUrl = EndpointSet,
    HasDeviceAuthUrl = EndpointNotSet,
    HasIntrospectionUrl = EndpointNotSet,
    HasRevocationUrl = EndpointNotSet,
    HasTokenUrl = EndpointSet,
> = OAuth2Client<
    BasicErrorResponse,
    GoogleTokenResponse,
    BasicTokenIntrospectionResponse,
    StandardRevocableToken,
    BasicRevocationErrorResponse,
    HasAuthUrl,
    HasDeviceAuthUrl,
    HasIntrospectionUrl,
    HasRevocationUrl,
    HasTokenUrl,
>;
