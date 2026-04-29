//! Shared helpers for merging an external JSON "patch" payload into a
//! provider credential resource.
//!
//! Every provider's `Resource::update_credential` follows the same recipe:
//!
//! 1. Re-serialize the incoming payload through `serde_json::Value` so that
//!    arbitrary JSON-shaped inputs (full credential dumps, OAuth token
//!    responses, hand-rolled `json!` macros) deserialize into a
//!    provider-specific `CredentialPatch` struct of `Option<_>` fields.
//! 2. For each field, replace the target only when the patch carries a value.
//! 3. Compute a new expiry from either an explicit `expiry` instant or a
//!    relative `expires_in` (seconds), preferring the relative form when both
//!    are present.
//!
//! These helpers extract the parts that do not depend on the concrete patch
//! struct, leaving each provider responsible only for declaring its field set.

use chrono::{DateTime, Duration, Utc};
use serde::{Serialize, de::DeserializeOwned};

use crate::error::PolluxError;

/// Re-serialize `payload` and decode it as the provider's `CredentialPatch`
/// struct.
///
/// Going through `serde_json::Value` lets the same entry point accept both
/// strongly-typed structs (e.g. `CodexResource` itself) and ad-hoc `json!`
/// objects without forcing every caller to commit to one shape.
pub(crate) fn parse_patch<P, T>(payload: T) -> Result<P, PolluxError>
where
    P: DeserializeOwned,
    T: Serialize,
{
    let value = serde_json::to_value(payload)?;
    let patch: P = serde_json::from_value(value)?;
    Ok(patch)
}

/// Overwrite a non-optional field when the patch supplies a value; leave the
/// existing value untouched otherwise.
pub(crate) fn set_plain<T>(target: &mut T, source: Option<T>) {
    if let Some(v) = source {
        *target = v;
    }
}

/// Overwrite an `Option` field when the patch supplies a value. A `None` in
/// the patch is treated as "field absent", not "clear the field".
pub(crate) fn set_opt<T>(target: &mut Option<T>, source: Option<T>) {
    if let Some(value) = source {
        *target = Some(value);
    }
}

/// Update an expiry timestamp from a patch.
///
/// `expires_in` (seconds, relative to now) takes precedence over the absolute
/// `expiry` instant — OAuth refresh responses always carry the relative form,
/// while persisted credential JSON tends to carry the absolute one.
pub(crate) fn apply_expiry(
    target: &mut DateTime<Utc>,
    expires_in: Option<i64>,
    expiry: Option<DateTime<Utc>>,
) {
    if let Some(secs) = expires_in {
        *target = Utc::now() + Duration::seconds(secs);
    } else if let Some(dt) = expiry {
        *target = dt;
    }
}
