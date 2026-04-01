use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, HashSet, VecDeque},
    fmt,
    time::{Duration, Instant},
};
use tracing::warn;

use crate::model_catalog::ModelCapabilities;
use crate::providers::codex::resource::CodexResource;
use crate::providers::manifest::CodexLease;

/// Result of evaluating a single credential candidate for a given model.
#[derive(Debug)]
pub(crate) enum LeaseStatus {
    /// Credential is usable — here is the lease.
    Ready(CodexLease),
    /// Credential exists but has expired and needs refreshing.
    Expired,
    /// Credential is in a rate-limit cooldown for this model.
    Cooling,
    /// Credential is already being refreshed.
    Refreshing,
    /// Credential does not support the requested model.
    Unsupported,
    /// Credential ID not found in the manager.
    Missing,
}

impl fmt::Display for LeaseStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LeaseStatus::Ready(lease) => write!(f, "ready(account={})", lease.account_id),
            LeaseStatus::Expired => f.write_str("expired"),
            LeaseStatus::Cooling => f.write_str("cooling"),
            LeaseStatus::Refreshing => f.write_str("refreshing"),
            LeaseStatus::Unsupported => f.write_str("unsupported"),
            LeaseStatus::Missing => f.write_str("missing"),
        }
    }
}

/// Runtime credential = base data + dynamic capabilities.
#[derive(Debug, Clone)]
pub struct RuntimeCredential {
    // Base credential data (persisted).
    pub inner: CodexResource,

    // Dynamic capability bitset (runtime-only unless persisted elsewhere).
    pub caps: ModelCapabilities,
}

impl RuntimeCredential {
    /// Constructor: assign initial capabilities on load.
    /// Typically `ModelCapabilities::all()` to start optimistic and disable on errors.
    pub fn new(inner: CodexResource, initial_caps: ModelCapabilities) -> Self {
        Self {
            inner,
            caps: initial_caps,
        }
    }

    /// Proxy: check expiration via the inner credential.
    #[inline(always)]
    pub fn is_expired(&self) -> bool {
        self.inner.is_expired()
    }
}

pub type CredentialId = u64;
pub type ModelIndex = usize;

#[derive(Debug, Clone, Copy)]
pub struct SchedulerStats {
    pub total_creds: usize,
    pub queue_len: usize,
    pub refreshing: usize,
    pub cooldowns: usize,
}

#[derive(Debug, Default)]
pub struct AssignmentResult {
    pub assigned: Option<CodexLease>,
    pub refresh_ids: Vec<CredentialId>,
    pub route_hit: bool,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CooldownTicket(Reverse<Instant>, CredentialId, ModelIndex);

/// Core scheduling logic for credentials (no IO, no locks).
pub struct CredentialManager {
    creds: HashMap<CredentialId, RuntimeCredential>,
    queues: Vec<VecDeque<CredentialId>>,
    waiting_room: BinaryHeap<CooldownTicket>,
    cooldown_map: HashMap<(CredentialId, ModelIndex), Instant>,
    refreshing: HashSet<CredentialId>,
}

impl Default for CredentialManager {
    fn default() -> Self {
        Self::new(0)
    }
}

impl CredentialManager {
    pub fn new(model_count: usize) -> Self {
        Self {
            creds: HashMap::new(),
            queues: vec![VecDeque::new(); model_count],
            waiting_room: BinaryHeap::new(),
            cooldown_map: HashMap::new(),
            refreshing: HashSet::new(),
        }
    }

    pub fn add_credential(
        &mut self,
        id: CredentialId,
        cred: CodexResource,
        initial_caps_bits: u64,
    ) {
        let initial_caps = ModelCapabilities::from_bits(initial_caps_bits);
        let caps = self
            .creds
            .get(&id)
            .map(|cred| cred.caps)
            .unwrap_or(initial_caps);

        self.creds.insert(id, RuntimeCredential::new(cred, caps));
        self.refreshing.remove(&id);

        for (index, queue) in self.queues.iter_mut().enumerate() {
            if !caps.supports(index) {
                continue;
            }

            if !queue.contains(&id) {
                queue.push_back(id);
            }
        }
    }

    /// Selects a credential for `model_mask`.
    ///
    /// When `sticky_id` is provided, it is evaluated first; on any non-ready
    /// status the method falls back to round-robin queue selection.
    /// Expired credentials encountered along either path are collected in
    /// [`AssignmentResult::refresh_ids`].
    pub fn get_assigned(
        &mut self,
        model_mask: u64,
        sticky_id: Option<CredentialId>,
    ) -> AssignmentResult {
        self.process_waiting_room();

        let mut result = AssignmentResult::default();

        let Some(model_index) = self.index_from_mask(model_mask) else {
            return result;
        };

        if let Some(id) = sticky_id {
            let status = self.check_lease(id, model_index);
            match status {
                LeaseStatus::Ready(lease) => {
                    result.assigned = Some(lease);
                    result.route_hit = true;
                    return result;
                }
                LeaseStatus::Expired => result.refresh_ids.push(id),
                _ => {}
            }
            if !matches!(status, LeaseStatus::Ready(_)) {
                warn!(id, %status, "[Codex] Sticky credential skipped");
            }
        }

        while let Some(id) = self.queues.get_mut(model_index).and_then(|q| q.pop_front()) {
            let status = self.check_lease(id, model_index);
            match status {
                LeaseStatus::Ready(lease) => {
                    if let Some(queue) = self.queues.get_mut(model_index) {
                        queue.push_back(id);
                    }
                    result.assigned = Some(lease);
                    return result;
                }
                LeaseStatus::Expired => result.refresh_ids.push(id),
                _ => {}
            }
            if !matches!(status, LeaseStatus::Ready(_)) {
                warn!(id, %status, "[Codex] Queue candidate skipped");
            }
        }
        result
    }

    fn process_waiting_room(&mut self) {
        let now = Instant::now();

        while self.waiting_room.peek().is_some_and(|t| (t.0).0 <= now) {
            let CooldownTicket(Reverse(ticket_deadline), credential_id, model_index) =
                self.waiting_room.pop().expect("peek guaranteed existence");

            match self.cooldown_map.entry((credential_id, model_index)) {
                std::collections::hash_map::Entry::Occupied(entry)
                    if ticket_deadline >= *entry.get() =>
                {
                    let ((reclaimed_cred_id, reclaimed_model_index), _) = entry.remove_entry();
                    if let Some(target_queue) = self.queues.get_mut(reclaimed_model_index)
                        && !target_queue.contains(&reclaimed_cred_id)
                    {
                        target_queue.push_back(reclaimed_cred_id);
                    }
                }
                _ => {}
            }
        }
    }

    pub fn mark_refreshing(&mut self, id: CredentialId) {
        self.refreshing.insert(id);
        self.clear_cooldowns_for(id);
    }

    pub fn mark_model_unsupported(
        &mut self,
        id: CredentialId,
        model_mask: u64,
    ) -> Option<(u64, u64)> {
        if model_mask == 0 {
            return None;
        }
        let cred = self.creds.get_mut(&id)?;
        let before = cred.caps.bits();
        cred.caps.disable_mask(model_mask);
        let after = cred.caps.bits();
        Some((before, after))
    }

    pub fn report_rate_limit(&mut self, id: CredentialId, model_mask: u64, cooldown: Duration) {
        let Some(model_index) = self.index_from_mask(model_mask) else {
            return;
        };
        let deadline = Instant::now() + cooldown;

        self.cooldown_map.insert((id, model_index), deadline);
        self.waiting_room
            .push(CooldownTicket(Reverse(deadline), id, model_index));
    }

    pub fn delete_credential(&mut self, id: CredentialId) {
        self.creds.remove(&id);
        self.refreshing.remove(&id);
        self.clear_cooldowns_for(id);
    }

    fn index_from_mask(&self, model_mask: u64) -> Option<ModelIndex> {
        if model_mask == 0 || (model_mask & (model_mask - 1)) != 0 {
            return None;
        }
        let index = model_mask.trailing_zeros() as usize;
        if index >= self.queues.len() {
            return None;
        }
        Some(index)
    }

    pub fn get_full_credential_copy(&self, id: CredentialId) -> Option<CodexResource> {
        self.creds.get(&id).map(|cred| cred.inner.clone())
    }

    pub fn account_id_of(&self, id: CredentialId) -> Option<String> {
        self.creds
            .get(&id)
            .map(|cred| cred.inner.account_id().to_string())
    }

    pub fn stats(&self, model_mask: u64) -> SchedulerStats {
        let queue_len = self
            .index_from_mask(model_mask)
            .and_then(|i| self.queues.get(i).map(|q| q.len()))
            .unwrap_or(0);

        SchedulerStats {
            total_creds: self.creds.len(),
            queue_len,
            refreshing: self.refreshing.len(),
            cooldowns: self.cooldown_map.len(),
        }
    }

    /// Single evaluation path for any credential candidate against a model index.
    fn check_lease(&self, id: CredentialId, model_index: ModelIndex) -> LeaseStatus {
        let Some(cred) = self.creds.get(&id) else {
            return LeaseStatus::Missing;
        };

        if !cred.caps.supports(model_index) {
            return LeaseStatus::Unsupported;
        }

        if self.refreshing.contains(&id) {
            return LeaseStatus::Refreshing;
        }

        if self.is_model_cooling(id, model_index) {
            return LeaseStatus::Cooling;
        }

        if cred.is_expired() {
            return LeaseStatus::Expired;
        }

        LeaseStatus::Ready(CodexLease {
            id,
            account_id: cred.inner.account_id().to_string(),
            access_token: cred.inner.access_token().to_string(),
        })
    }

    pub fn contains(&self, id: CredentialId) -> bool {
        self.creds.contains_key(&id)
    }

    pub fn is_refreshing(&self, id: CredentialId) -> bool {
        self.refreshing.contains(&id)
    }

    fn is_model_cooling(&self, id: CredentialId, model_index: ModelIndex) -> bool {
        match self.cooldown_map.get(&(id, model_index)) {
            Some(deadline) => Instant::now() < *deadline,
            None => false,
        }
    }

    fn clear_cooldowns_for(&mut self, id: CredentialId) {
        self.cooldown_map.retain(|(cid, _), _| *cid != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use serde_json::json;

    fn make_credential(account_id: &str) -> CodexResource {
        CodexResource::from_payload(json!({
            "email": null,
            "account_id": account_id,
            "refresh_token": "refresh",
            "access_token": "token",
            "expiry": Utc::now() + Duration::minutes(10),
        }))
        .expect("valid resource payload")
    }

    fn make_expired_credential(account_id: &str) -> CodexResource {
        let mut cred = make_credential(account_id);
        cred.update_credential(json!({
            "expiry": Utc::now() - Duration::minutes(10),
        }))
        .expect("valid expiry update");
        cred
    }

    fn mask(index: usize) -> u64 {
        1u64 << index
    }

    #[test]
    fn cooldown_blocks_and_requeues() {
        let mut manager = CredentialManager::new(1);

        let mut caps = ModelCapabilities::none();
        caps.enable(0);
        manager.add_credential(1, make_credential("acct1"), caps.bits());

        manager.report_rate_limit(1, mask(0), std::time::Duration::from_millis(10));

        let assigned_during_cooldown = manager.get_assigned(mask(0), None).assigned;
        assert!(assigned_during_cooldown.is_none());

        std::thread::sleep(std::time::Duration::from_millis(30));

        let assigned_after = manager
            .get_assigned(mask(0), None)
            .assigned
            .expect("assigned after cooldown");
        assert_eq!(assigned_after.account_id, "acct1");
    }

    #[test]
    fn expired_credential_triggers_refresh_request() {
        let mut manager = CredentialManager::new(1);
        let mut caps = ModelCapabilities::none();
        caps.enable(0);

        manager.add_credential(1, make_expired_credential("acct1"), caps.bits());

        let result = manager.get_assigned(mask(0), None);
        assert!(result.assigned.is_none());
        assert_eq!(result.refresh_ids, vec![1]);
    }

    #[test]
    fn mark_model_unsupported_keeps_credential_available_for_other_models() {
        let mut manager = CredentialManager::new(2);

        let mut caps = ModelCapabilities::none();
        caps.enable(0);
        caps.enable(1);
        manager.add_credential(1, make_credential("acct1"), caps.bits());

        manager.mark_model_unsupported(1, mask(1));

        let unsupported = manager.get_assigned(mask(1), None);
        assert!(unsupported.assigned.is_none());
        assert!(unsupported.refresh_ids.is_empty());
        assert_eq!(manager.stats(mask(1)).queue_len, 0);
        assert_eq!(manager.stats(mask(0)).queue_len, 1);

        let supported = manager
            .get_assigned(mask(0), None)
            .assigned
            .expect("assigned for supported model");
        assert_eq!(supported.account_id, "acct1");
    }

    #[test]
    fn cooldown_is_per_model() {
        let mut manager = CredentialManager::new(2);

        let mut caps = ModelCapabilities::none();
        caps.enable(0);
        caps.enable(1);
        manager.add_credential(1, make_credential("acct1"), caps.bits());

        manager.report_rate_limit(1, mask(0), std::time::Duration::from_secs(60));

        let assigned_during_cooldown = manager.get_assigned(mask(0), None).assigned;
        assert!(assigned_during_cooldown.is_none());
        assert_eq!(manager.stats(mask(0)).queue_len, 0);

        let assigned_other_model = manager
            .get_assigned(mask(1), None)
            .assigned
            .expect("assigned for other model during cooldown");
        assert_eq!(assigned_other_model.account_id, "acct1");

        assert_eq!(manager.stats(mask(1)).queue_len, 1);
    }

    // ── hint path tests ──

    #[test]
    fn hint_valid_credential_returns_lease_with_route_hit() {
        let mut manager = CredentialManager::new(1);
        let mut caps = ModelCapabilities::none();
        caps.enable(0);
        manager.add_credential(1, make_credential("acct1"), caps.bits());

        let result = manager.get_assigned(mask(0), Some(1));
        assert!(result.route_hit);
        let lease = result.assigned.expect("hint should produce a lease");
        assert_eq!(lease.id, 1);
        assert_eq!(lease.account_id, "acct1");
    }

    #[test]
    fn hint_expired_triggers_refresh_and_falls_back() {
        let mut manager = CredentialManager::new(1);
        let mut caps = ModelCapabilities::none();
        caps.enable(0);
        manager.add_credential(1, make_expired_credential("acct1"), caps.bits());
        manager.add_credential(2, make_credential("acct2"), caps.bits());

        let result = manager.get_assigned(mask(0), Some(1));
        assert!(!result.route_hit);
        assert!(result.refresh_ids.contains(&1));
        let lease = result.assigned.expect("should fall back to queue");
        assert_eq!(lease.account_id, "acct2");
    }

    #[test]
    fn hint_cooling_falls_back_to_queue() {
        let mut manager = CredentialManager::new(1);
        let mut caps = ModelCapabilities::none();
        caps.enable(0);
        manager.add_credential(1, make_credential("acct1"), caps.bits());
        manager.add_credential(2, make_credential("acct2"), caps.bits());

        manager.report_rate_limit(1, mask(0), std::time::Duration::from_secs(60));

        let result = manager.get_assigned(mask(0), Some(1));
        assert!(!result.route_hit);
        assert!(result.refresh_ids.is_empty());
        let lease = result.assigned.expect("should fall back to queue");
        assert_eq!(lease.account_id, "acct2");
    }

    #[test]
    fn hint_refreshing_no_duplicate_refresh_falls_back() {
        let mut manager = CredentialManager::new(1);
        let mut caps = ModelCapabilities::none();
        caps.enable(0);
        manager.add_credential(1, make_credential("acct1"), caps.bits());
        manager.add_credential(2, make_credential("acct2"), caps.bits());

        manager.mark_refreshing(1);

        let result = manager.get_assigned(mask(0), Some(1));
        assert!(!result.route_hit);
        assert!(
            !result.refresh_ids.contains(&1),
            "should not duplicate refresh"
        );
        let lease = result.assigned.expect("should fall back to queue");
        assert_eq!(lease.account_id, "acct2");
    }

    #[test]
    fn hint_removed_falls_back_to_queue() {
        let mut manager = CredentialManager::new(1);
        let mut caps = ModelCapabilities::none();
        caps.enable(0);
        manager.add_credential(2, make_credential("acct2"), caps.bits());

        // hint points to non-existent credential
        let result = manager.get_assigned(mask(0), Some(999));
        assert!(!result.route_hit);
        let lease = result.assigned.expect("should fall back to queue");
        assert_eq!(lease.account_id, "acct2");
    }

    #[test]
    fn hint_unsupported_model_falls_back_to_queue() {
        let mut manager = CredentialManager::new(2);
        let mut caps_0 = ModelCapabilities::none();
        caps_0.enable(0);
        let mut caps_1 = ModelCapabilities::none();
        caps_1.enable(1);

        manager.add_credential(1, make_credential("acct1"), caps_0.bits());
        manager.add_credential(2, make_credential("acct2"), caps_1.bits());

        // hint credential 1 for model 1, but cred 1 only supports model 0
        let result = manager.get_assigned(mask(1), Some(1));
        assert!(!result.route_hit);
        let lease = result.assigned.expect("should fall back to queue");
        assert_eq!(lease.account_id, "acct2");
    }
}
