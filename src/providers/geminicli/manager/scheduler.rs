use crate::model_catalog::ModelCapabilities;
use crate::providers::geminicli::resource::GeminiCliResource;
use crate::providers::manifest::GeminiCliLease;

type LeaseStatus = crate::providers::lease_status::LeaseStatus<GeminiCliLease>;
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

/// Runtime credential = base data + dynamic capabilities.
#[derive(Debug, Clone)]
pub struct RuntimeCredential {
    // Base credential data (persisted).
    pub inner: GeminiCliResource,

    // Dynamic capability bitset (runtime-only unless persisted elsewhere).
    pub caps: ModelCapabilities,
}

impl RuntimeCredential {
    /// Constructor: assign initial capabilities on load.
    /// Typically `ModelCapabilities::all()` to start optimistic and disable on errors.
    pub fn new(inner: GeminiCliResource, initial_caps: ModelCapabilities) -> Self {
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

#[derive(Debug, Clone, Copy, Default)]
pub struct SchedulerStats {
    pub total_creds: usize,
    pub queue_len: usize,
    pub refreshing: usize,
    pub cooldowns: usize,
    pub skipped_cooling: usize,
    pub skipped_refreshing: usize,
    pub skipped_unsupported: usize,
    pub skipped_expired: usize,
}

#[derive(Debug, Default)]
pub struct AssignmentResult {
    pub assigned: Option<GeminiCliLease>,
    pub refresh_ids: Vec<CredentialId>,
    pub stats: SchedulerStats,
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
        cred: GeminiCliResource,
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

    pub fn delete_credential(&mut self, id: CredentialId) {
        self.creds.remove(&id);
        self.refreshing.remove(&id);
        self.clear_cooldowns_for(id);
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

    pub fn get_full_credential_copy(&self, id: CredentialId) -> Option<GeminiCliResource> {
        self.creds.get(&id).map(|cred| cred.inner.clone())
    }

    pub fn project_id_of(&self, id: CredentialId) -> Option<String> {
        self.creds
            .get(&id)
            .map(|cred| cred.inner.project_id().to_string())
    }

    pub fn contains(&self, id: CredentialId) -> bool {
        self.creds.contains_key(&id)
    }

    pub fn get_assigned(&mut self, model_mask: u64) -> AssignmentResult {
        self.process_waiting_room();

        let mut result = AssignmentResult::default();

        let Some(model_index) = self.index_from_mask(model_mask) else {
            return result;
        };

        result.stats = SchedulerStats {
            total_creds: self.creds.len(),
            queue_len: self.queues.get(model_index).map_or(0, |q| q.len()),
            refreshing: self.refreshing.len(),
            cooldowns: self.cooldown_map.len(),
            ..Default::default()
        };

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
                LeaseStatus::Expired => {
                    result.refresh_ids.push(id);
                    result.stats.skipped_expired += 1;
                }
                LeaseStatus::Cooling => result.stats.skipped_cooling += 1,
                LeaseStatus::Refreshing => result.stats.skipped_refreshing += 1,
                LeaseStatus::Unsupported => result.stats.skipped_unsupported += 1,
                LeaseStatus::Missing => {}
            }
        }
        result
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

        LeaseStatus::Ready(GeminiCliLease {
            id,
            project_id: cred.inner.project_id().to_string(),
            access_token: cred.inner.access_token().to_owned(),
            email: cred.inner.email().map(str::to_owned),
        })
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

    pub fn total_creds(&self) -> usize {
        self.creds.len()
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

    fn make_credential(project_id: &str) -> GeminiCliResource {
        GeminiCliResource::from_payload(json!({
            "email": null,
            "project_id": project_id,
            "refresh_token": "refresh",
            "access_token": "token",
            "expiry": Utc::now() + Duration::minutes(10),
        }))
        .expect("valid resource payload")
    }

    fn make_expired_credential(project_id: &str) -> GeminiCliResource {
        let mut cred = make_credential(project_id);
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
    fn add_credential_respects_capabilities() {
        let mut manager = CredentialManager::new(2);
        let mut caps = ModelCapabilities::none();
        caps.enable(0);

        manager.add_credential(1, make_credential("p1"), caps.bits());

        let assigned = manager
            .get_assigned(mask(0))
            .assigned
            .expect("assigned for model 0");
        assert_eq!(assigned.project_id, "p1");

        let assigned_other = manager.get_assigned(mask(1)).assigned;
        assert!(assigned_other.is_none());
    }

    #[test]
    fn mark_model_unsupported_disables_capability() {
        let mut manager = CredentialManager::new(2);

        manager.add_credential(1, make_credential("p1"), ModelCapabilities::all().bits());
        manager.mark_model_unsupported(1, mask(1));

        let assigned_blocked = manager.get_assigned(mask(1)).assigned;
        assert!(assigned_blocked.is_none());

        let assigned_allowed = manager
            .get_assigned(mask(0))
            .assigned
            .expect("assigned for model 0");
        assert_eq!(assigned_allowed.project_id, "p1");
    }

    #[test]
    fn cooldown_blocks_and_requeues() {
        let mut manager = CredentialManager::new(1);

        let mut caps = ModelCapabilities::none();
        caps.enable(0);
        manager.add_credential(1, make_credential("p1"), caps.bits());

        manager.report_rate_limit(1, mask(0), std::time::Duration::from_millis(10));

        let assigned_during_cooldown = manager.get_assigned(mask(0)).assigned;
        assert!(assigned_during_cooldown.is_none());

        std::thread::sleep(std::time::Duration::from_millis(20));

        let assigned_after = manager
            .get_assigned(mask(0))
            .assigned
            .expect("assigned after cooldown");
        assert_eq!(assigned_after.project_id, "p1");
    }

    #[test]
    fn expired_token_triggers_refresh_request() {
        let mut manager = CredentialManager::new(1);
        let mut caps = ModelCapabilities::none();
        caps.enable(0);

        manager.add_credential(1, make_expired_credential("p1"), caps.bits());

        let result = manager.get_assigned(mask(0));
        assert!(result.assigned.is_none());
        assert_eq!(result.refresh_ids, vec![1]);
    }

    #[test]
    fn refreshing_credential_is_skipped() {
        let mut manager = CredentialManager::new(1);
        let mut caps = ModelCapabilities::none();
        caps.enable(0);

        manager.add_credential(1, make_credential("p1"), caps.bits());
        manager.add_credential(2, make_credential("p2"), caps.bits());

        manager.mark_refreshing(1);

        let assigned = manager
            .get_assigned(mask(0))
            .assigned
            .expect("assigned for model 0");
        assert_eq!(assigned.id, 2);
    }

    #[test]
    fn readd_after_refresh_preserves_disabled_caps() {
        let mut manager = CredentialManager::new(2);

        manager.add_credential(1, make_credential("p1"), ModelCapabilities::all().bits());
        manager.mark_model_unsupported(1, mask(1));

        manager.add_credential(1, make_credential("p1"), ModelCapabilities::all().bits());

        let assigned_blocked = manager.get_assigned(mask(1)).assigned;
        assert!(assigned_blocked.is_none());

        let assigned_allowed = manager
            .get_assigned(mask(0))
            .assigned
            .expect("assigned for model 0");
        assert_eq!(assigned_allowed.id, 1);
    }

    #[test]
    fn multiple_credentials_rotate_in_queue() {
        let mut manager = CredentialManager::new(1);
        let mut caps = ModelCapabilities::none();
        caps.enable(0);

        manager.add_credential(1, make_credential("p1"), caps.bits());
        manager.add_credential(2, make_credential("p2"), caps.bits());

        let first = manager
            .get_assigned(mask(0))
            .assigned
            .expect("first assignment");
        let second = manager
            .get_assigned(mask(0))
            .assigned
            .expect("second assignment");

        assert_eq!(first.id, 1);
        assert_eq!(second.id, 2);
    }

    #[test]
    fn cooldown_requeue_does_not_duplicate() {
        let mut manager = CredentialManager::new(1);
        let mut caps = ModelCapabilities::none();
        caps.enable(0);

        manager.add_credential(1, make_credential("p1"), caps.bits());

        // Simulate: get_assigned pops and push_backs (credential is in queue)
        let assigned = manager.get_assigned(mask(0)).assigned;
        assert!(assigned.is_some());

        // 429 arrives -> short cooldown
        manager.report_rate_limit(1, mask(0), std::time::Duration::from_millis(10));

        // Wait for cooldown to expire
        std::thread::sleep(std::time::Duration::from_millis(20));

        // process_waiting_room should NOT duplicate the credential
        let result = manager.get_assigned(mask(0));
        let queue_len = result.stats.queue_len;

        // Queue should contain exactly 1 copy, not 2
        assert_eq!(
            queue_len, 1,
            "credential duplicated in queue: len={queue_len}"
        );
    }
}
