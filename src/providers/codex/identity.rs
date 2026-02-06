use crate::error::PolluxError;
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) struct CodexIdentity {
    pub(crate) account_id: String,
    pub(crate) sub: String,
    pub(crate) email: Option<String>,
    pub(crate) chatgpt_plan_type: Option<String>,
}

pub(crate) fn identity_from_id_token(id_token: &str) -> Result<CodexIdentity, PolluxError> {
    let claims = crate::utils::jwt::decode_jwt_claims(id_token).ok_or_else(|| {
        PolluxError::UnexpectedError("Failed to decode id_token claims".to_string())
    })?;

    let sub = claims
        .get("sub")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| {
            PolluxError::UnexpectedError("Missing sub in id_token claims".to_string())
        })?;

    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .map(ToString::to_string);

    let auth_obj = claims
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object);

    let account_id = auth_obj
        .and_then(|o| o.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| {
            PolluxError::UnexpectedError(
                "Missing https://api.openai.com/auth.chatgpt_account_id in id_token claims"
                    .to_string(),
            )
        })?;

    let chatgpt_plan_type = auth_obj
        .and_then(|o| o.get("chatgpt_plan_type"))
        .and_then(Value::as_str)
        .map(ToString::to_string);

    Ok(CodexIdentity {
        account_id,
        sub,
        email,
        chatgpt_plan_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth_utils::OauthTokenResponse;
    use base64::Engine as _;
    use serde_json::json;

    fn make_jwt(payload: &serde_json::Value) -> String {
        // Signature is intentionally irrelevant: production code only decodes the JWT payload.
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload_bytes = serde_json::to_vec(payload).expect("serialize payload");
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload_bytes);
        format!("{header}.{payload_b64}.sig")
    }

    fn test_claims() -> serde_json::Value {
        json!({
            "sub": "auth0|test-subject",
            "email": "user@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-test-id",
                "chatgpt_plan_type": "plus",
            }
        })
    }

    #[test]
    fn decode_jwt_claims_decodes_payload() {
        let claims = test_claims();
        let id_token = make_jwt(&claims);

        let token_response: OauthTokenResponse = serde_json::from_value(json!({
            "access_token": "at-test",
            "token_type": "bearer",
            "expires_in": 3600,
            "refresh_token": "rt-test",
            "id_token": id_token,
        }))
        .expect("token response deserializes");

        let id_token = token_response
            .extra_fields()
            .id_token
            .as_deref()
            .expect("token response includes id_token");

        let parsed = crate::utils::jwt::decode_jwt_claims(id_token).expect("claims parsed");
        assert_eq!(parsed, claims);
    }

    #[test]
    fn identity_from_id_token_extracts_expected_fields() {
        let claims = test_claims();
        let id_token = make_jwt(&claims);

        let identity = identity_from_id_token(&id_token).expect("identity resolved");
        assert_eq!(identity.account_id, "acct-test-id");
        assert_eq!(identity.sub, "auth0|test-subject");
        assert_eq!(identity.email, Some("user@example.com".to_string()));
        assert_eq!(identity.chatgpt_plan_type, Some("plus".to_string()));
    }
}
