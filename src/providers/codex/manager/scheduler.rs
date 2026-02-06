use crate::model_catalog::ModelCapabilities;
use crate::providers::codex::resource::CodexResource;
use crate::providers::manifest::CodexLease;
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

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

#[derive(Debug, Default)]
pub struct AssignmentResult {
    pub assigned: Option<CodexLease>,
    pub refresh_ids: Vec<CredentialId>,
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

    pub fn get_assigned(&mut self, model_mask: u64) -> AssignmentResult {
        self.process_waiting_room();

        let mut result = AssignmentResult::default();

        let Some(model_index) = self.index_from_mask(model_mask) else {
            return result;
        };

        while let Some(id) = self.queues.get_mut(model_index).and_then(|q| q.pop_front()) {
            let Some(cred) = self.creds.get(&id) else {
                continue;
            };

            if !cred.caps.supports(model_index) {
                continue;
            }

            if self.refreshing.contains(&id) || self.is_model_cooling(id, model_index) {
                continue;
            }

            if cred.is_expired() {
                result.refresh_ids.push(id);
                continue;
            }

            let token = cred.inner.access_token().to_string();

            if let Some(queue) = self.queues.get_mut(model_index) {
                queue.push_back(id);
            }

            result.assigned = Some(CodexLease {
                id,
                account_id: cred.inner.account_id().to_string(),
                access_token: token,
            });
            return result;
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

    pub fn contains(&self, id: CredentialId) -> bool {
        self.creds.contains_key(&id)
    }

    pub fn queue_len(&self, model_mask: u64) -> usize {
        self.index_from_mask(model_mask)
            .and_then(|model_index| self.queues.get(model_index).map(|q| q.len()))
            .unwrap_or(0)
    }

    pub fn total_creds(&self) -> usize {
        self.creds.len()
    }

    pub fn refreshing_len(&self) -> usize {
        self.refreshing.len()
    }

    pub fn is_refreshing(&self, id: CredentialId) -> bool {
        self.refreshing.contains(&id)
    }

    pub fn cooldown_len(&self) -> usize {
        self.cooldown_map.len()
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

        let assigned_during_cooldown = manager.get_assigned(mask(0)).assigned;
        assert!(assigned_during_cooldown.is_none());

        std::thread::sleep(std::time::Duration::from_millis(30));

        let assigned_after = manager
            .get_assigned(mask(0))
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

        let result = manager.get_assigned(mask(0));
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

        let unsupported = manager.get_assigned(mask(1));
        assert!(unsupported.assigned.is_none());
        assert!(unsupported.refresh_ids.is_empty());
        assert_eq!(manager.queue_len(mask(1)), 0);
        assert_eq!(manager.queue_len(mask(0)), 1);

        let supported = manager
            .get_assigned(mask(0))
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

        let assigned_during_cooldown = manager.get_assigned(mask(0)).assigned;
        assert!(assigned_during_cooldown.is_none());
        assert_eq!(manager.queue_len(mask(0)), 0);

        let assigned_other_model = manager
            .get_assigned(mask(1))
            .assigned
            .expect("assigned for other model during cooldown");
        assert_eq!(assigned_other_model.account_id, "acct1");

        assert_eq!(manager.queue_len(mask(1)), 1);
    }
}
