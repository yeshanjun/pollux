use crate::providers::traits::scheduler::CredentialId;
use moka::sync::Cache;
use std::time::Duration;

/// Session-affinity credential routing cache.
///
/// Pins `(route_key, model_mask)` → [`CredentialId`] so that consecutive requests
/// within the same conversation reuse the same upstream account, improving
/// prompt-cache hit rates on the provider side.
///
/// Stale entries are handled lazily — the scheduler evaluates each hinted ID
/// via [`ResourceScheduler::get_assigned`] and falls back to queue selection on miss.
pub struct RouteTable {
    cache: Cache<(u64, u64), CredentialId>,
}

impl RouteTable {
    /// Creates a table bounded by `max_capacity` entries with the given idle TTL.
    pub fn new(max_capacity: u64, ttl: Duration) -> Self {
        let cache = Cache::builder()
            .max_capacity(max_capacity)
            .time_to_idle(ttl)
            .build();
        Self { cache }
    }

    /// Returns the cached credential for this `(session, model)` pair, if any.
    #[inline]
    pub fn get(&self, route_key: u64, model_mask: u64) -> Option<CredentialId> {
        self.cache.get(&(route_key, model_mask))
    }

    /// Binds a `(session, model)` pair to the given credential.
    #[inline]
    pub fn insert(&self, route_key: u64, model_mask: u64, credential_id: CredentialId) {
        self.cache.insert((route_key, model_mask), credential_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let rt = RouteTable::new(100, Duration::from_mins(1));
        rt.insert(0xABCD, 0x01, 42);
        assert_eq!(rt.get(0xABCD, 0x01), Some(42));
        assert_eq!(rt.get(0xABCD, 0x02), None);
        assert_eq!(rt.get(0x1234, 0x01), None);
    }

    #[test]
    fn different_model_mask_same_session() {
        let rt = RouteTable::new(100, Duration::from_mins(1));
        rt.insert(0xAA, 0x01, 100);
        rt.insert(0xAA, 0x02, 200);
        assert_eq!(rt.get(0xAA, 0x01), Some(100));
        assert_eq!(rt.get(0xAA, 0x02), Some(200));
    }
}
