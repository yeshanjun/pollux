use crate::error::PolluxError;
use oauth2::basic::{
    BasicErrorResponse, BasicRevocationErrorResponse, BasicTokenIntrospectionResponse,
    BasicTokenType,
};
use oauth2::{
    AuthUrl, Client as OAuth2Client, ClientId, ClientSecret, ExtraTokenFields, RedirectUrl,
    StandardRevocableToken, StandardTokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Extra (non-standard) OAuth token response fields.
///
/// We keep OpenID Connect's `id_token` plus any additional JSON fields via `flatten` for forward
/// compatibility. Debug output is redacted to avoid leaking secrets.
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct CustomTokenFields {
    pub id_token: Option<String>,

    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl ExtraTokenFields for CustomTokenFields {}

impl std::fmt::Debug for CustomTokenFields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let id_token = self.id_token.as_ref().map(|_| "<redacted>");
        let mut keys: Vec<&String> = self.extra.keys().collect();
        keys.sort();

        f.debug_struct("CustomTokenFields")
            .field("id_token", &id_token)
            .field("extra_keys", &keys)
            .finish()
    }
}

/// Standard OAuth2 token endpoint response extended with [`CustomTokenFields`].
pub(crate) type OauthTokenResponse = StandardTokenResponse<CustomTokenFields, BasicTokenType>;

/// A standard OAuth2 client configured to return [`OauthTokenResponse`].
pub(crate) type StandardOauth2Client<
    HasAuthUrl = oauth2::EndpointSet,
    HasDeviceAuthUrl = oauth2::EndpointNotSet,
    HasIntrospectionUrl = oauth2::EndpointNotSet,
    HasRevocationUrl = oauth2::EndpointNotSet,
    HasTokenUrl = oauth2::EndpointSet,
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

/// Build a standard OAuth2 client for `authorization_code` + `refresh_token` flows.
///
/// This intentionally lives at the crate boundary (not inside provider modules) to keep OAuth
/// glue reusable and dependency flow one-way (providers -> oauth_utils).
pub(crate) fn build_oauth2_client(
    client_id: &str,
    client_secret: Option<&str>,
    auth_url: &str,
    token_url: &str,
    redirect_url: RedirectUrl,
) -> Result<StandardOauth2Client, PolluxError> {
    let mut client = OAuth2Client::<
        BasicErrorResponse,
        OauthTokenResponse,
        BasicTokenIntrospectionResponse,
        StandardRevocableToken,
        BasicRevocationErrorResponse,
    >::new(ClientId::new(client_id.to_string()));

    if let Some(secret) = client_secret {
        client = client.set_client_secret(ClientSecret::new(secret.to_string()));
    }

    let client = client
        .set_auth_uri(AuthUrl::new(auth_url.to_string())?)
        .set_token_uri(TokenUrl::new(token_url.to_string())?)
        .set_redirect_uri(redirect_url);

    Ok(client)
}
