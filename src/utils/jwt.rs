use base64::Engine as _;
use serde_json::Value;

/// Decode the payload JSON ("claims") from a JWT.
///
/// This is intentionally signature-agnostic: it does not validate the JWT,
/// it only base64url-decodes the payload segment and parses it as JSON.
pub(crate) fn decode_jwt_claims(jwt: &str) -> Option<Value> {
    let payload_b64 = jwt.split('.').nth(1)?;

    // Most JWTs are base64url without padding, but some toolchains may include padding.
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload_b64))
        .ok()?;

    serde_json::from_slice(&bytes).ok()
}
