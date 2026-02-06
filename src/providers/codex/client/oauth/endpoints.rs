use crate::error::OauthError;
use crate::oauth_utils::{OauthTokenResponse, build_oauth2_client};
use oauth2::{
    AuthorizationCode, Client as OAuth2Client, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RefreshToken, Scope, StandardRevocableToken,
    basic::{BasicErrorResponse, BasicRevocationErrorResponse, BasicTokenIntrospectionResponse},
};
use std::sync::LazyLock;
use tracing::info;

/// Stateless OpenAI OAuth endpoints for the Codex CLI flow.
pub(crate) struct CodexOauthEndpoints;

/// Fixed Codex CLI OAuth client id (public client, no secret).
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Fixed OpenAI OAuth endpoints (not configurable).
///
/// This matches the Codex CLI flow and keeps the auth/token server stable.
const OPENAI_AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

const DEFAULT_ORIGINATOR: &str = "codex_cli_rs";

static OAUTH_CALLBACK_URL: LazyLock<RedirectUrl> = LazyLock::new(|| {
    // NOTE: This callback must match the OAuth app's pre-registered redirect URL for
    // `CODEX_CLIENT_ID`. Codex CLI uses a fixed local callback server on port 1455.
    RedirectUrl::new("http://localhost:1455/auth/callback".to_string())
        .expect("valid OAuth callback URL bound to localhost")
});

pub(crate) static DEFAULT_SCOPES: LazyLock<Vec<Scope>> = LazyLock::new(|| {
    vec![
        Scope::new("openid".to_string()),
        Scope::new("profile".to_string()),
        Scope::new("email".to_string()),
        Scope::new("offline_access".to_string()),
    ]
});

pub(crate) static CODEX_OAUTH_CLIENT: LazyLock<CodexOauth2Client> = LazyLock::new(|| {
    build_oauth2_client(
        CODEX_CLIENT_ID,
        None,
        OPENAI_AUTH_URL,
        OPENAI_TOKEN_URL,
        OAUTH_CALLBACK_URL.clone(),
    )
    .expect("valid Codex OAuth2 client with redirect")
});

impl CodexOauthEndpoints {
    pub(crate) fn client() -> &'static CodexOauth2Client {
        &CODEX_OAUTH_CLIENT
    }

    pub(crate) fn build_authorize_url(pkce_challenge: PkceCodeChallenge) -> (url::Url, CsrfToken) {
        let mut req = Self::client()
            .authorize_url(CsrfToken::new_random)
            .set_pkce_challenge(pkce_challenge)
            .add_extra_param("id_token_add_organizations", "true")
            .add_extra_param("codex_cli_simplified_flow", "true")
            .add_extra_param("originator", DEFAULT_ORIGINATOR);

        for scope in DEFAULT_SCOPES.iter() {
            req = req.add_scope(scope.clone());
        }

        req.url()
    }

    pub(crate) async fn exchange_authorization_code(
        code: AuthorizationCode,
        verifier: PkceCodeVerifier,
        http_client: reqwest::Client,
    ) -> Result<OauthTokenResponse, OauthError> {
        let token_result: OauthTokenResponse = Self::client()
            .exchange_code(code)
            .set_pkce_verifier(verifier)
            .request_async(&http_client)
            .await?;
        info!("Codex OAuth2 code exchange completed successfully");
        Ok(token_result)
    }

    #[allow(dead_code)]
    pub(crate) async fn refresh_access_token(
        refresh_token: &str,
        http_client: reqwest::Client,
    ) -> Result<OauthTokenResponse, OauthError> {
        let token_result: OauthTokenResponse = Self::client()
            .exchange_refresh_token(&RefreshToken::new(refresh_token.to_string()))
            .request_async(&http_client)
            .await?;
        Ok(token_result)
    }
}

pub(crate) type CodexOauth2Client<
    HasAuthUrl = EndpointSet,
    HasDeviceAuthUrl = EndpointNotSet,
    HasIntrospectionUrl = EndpointNotSet,
    HasRevocationUrl = EndpointNotSet,
    HasTokenUrl = EndpointSet,
> = OAuth2Client<
    BasicErrorResponse,
    OauthTokenResponse,
    BasicTokenIntrospectionResponse,
    StandardRevocableToken,
    BasicRevocationErrorResponse,
    HasAuthUrl,
    HasDeviceAuthUrl,
    HasIntrospectionUrl,
    HasRevocationUrl,
    HasTokenUrl,
>;
