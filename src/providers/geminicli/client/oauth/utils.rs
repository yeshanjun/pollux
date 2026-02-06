use serde_json::Value;

/// Insert identity claims (`email`, `sub`) into the token payload from the embedded `id_token`, if
/// available.
pub fn attach_email_from_id_token(token_value: &mut Value) {
    let claims = token_value
        .get("id_token")
        .and_then(Value::as_str)
        .and_then(crate::utils::jwt::decode_jwt_claims);

    let sub = claims
        .as_ref()
        .and_then(|payload| payload.get("sub"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    let email = claims
        .as_ref()
        .and_then(|payload| payload.get("email"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    if let Some(obj) = token_value.as_object_mut() {
        if let Some(sub) = sub {
            obj.insert("sub".to_string(), Value::String(sub));
        }
        if let Some(email) = email {
            obj.insert("email".to_string(), Value::String(email));
        }
    }
}
