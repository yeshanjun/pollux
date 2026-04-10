use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::fmt::Debug;
use std::time::{Duration, Instant};

use super::lease_status::{LeaseLabel, LeaseStatus};
use crate::model_catalog::ModelCapabilities;

pub type CredentialId = u64;
pub type ModelIndex = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CooldownScope {
    /// Cooldown applies only to the model that triggered the 429.
    PerModel,
    /// Cooldown applies to all models under the affected credential.
    PerCredential,
}

/// Trait for credential resources that can be managed by the generic scheduler.
///
/// Each provider implements this on its resource type (e.g. `GeminiCliResource`,
/// `CodexResource`) to plug into the shared `CredentialManager<R>` scheduling logic.
pub trait Schedulable: Clone + Debug {
    /// The lease type produced on successful credential assignment.
    type Lease: LeaseLabel + Debug;

    /// Cooldown granularity for this provider.
    /// Defaults to `PerModel` (per-model independent cooldown).
    const COOLDOWN_GRANULARITY: CooldownScope = CooldownScope::PerModel;

    /// Check if the credential has expired and needs token refresh.
    fn is_expired(&self) -> bool;

    /// Build a lease from this resource for the given credential ID.
    fn make_lease(&self, id: CredentialId) -> Self::Lease;
}

/// Runtime credential = base resource data + dynamic capability bitset.
#[derive(Debug, Clone)]
pub struct RuntimeCredential<R> {
    pub inner: R,
    pub caps: ModelCapabilities,
}

impl<R: Schedulable> RuntimeCredential<R> {
    pub fn new(inner: R, initial_caps: ModelCapabilities) -> Self {
        Self {
            inner,
            caps: initial_caps,
        }
    }
}

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

#[derive(Debug)]
pub struct AssignmentResult<L> {
    pub assigned: Option<L>,
    pub refresh_ids: Vec<CredentialId>,
    pub route_hit: bool,
    pub stats: SchedulerStats,
}

impl<L> Default for AssignmentResult<L> {
    fn default() -> Self {
        Self {
            assigned: None,
            refresh_ids: Vec::new(),
            route_hit: false,
            stats: SchedulerStats::default(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CooldownTicket(Reverse<Instant>, CredentialId, ModelIndex);

#[derive(Debug, Default, Clone)]
struct ModelQueue {
    order: VecDeque<CredentialId>,
    members: HashSet<CredentialId>,
}

impl ModelQueue {
    fn push_back(&mut self, id: CredentialId) {
        if self.members.insert(id) {
            self.order.push_back(id);
        }
    }

    fn pop_front(&mut self) -> Option<CredentialId> {
        let id = self.order.pop_front()?;
        self.members.remove(&id);
        Some(id)
    }

    fn len(&self) -> usize {
        self.order.len()
    }
}

/// Generic credential scheduler. Pure logic, no IO, no locks.
///
/// All provider-specific scheduling (GeminiCli, Codex, Antigravity, …) is
/// unified here. The type parameter `R` controls what a "credential" looks
/// like and how it produces a lease.
pub struct CredentialManager<R: Schedulable> {
    creds: HashMap<CredentialId, RuntimeCredential<R>>,
    queues: Vec<ModelQueue>,
    waiting_room: BinaryHeap<CooldownTicket>,
    cooldown_map: HashMap<(CredentialId, ModelIndex), Instant>,
    refreshing: HashSet<CredentialId>,
}

impl<R: Schedulable> Default for CredentialManager<R> {
    fn default() -> Self {
        Self::new(0)
    }
}

impl<R: Schedulable> CredentialManager<R> {
    pub fn new(model_count: usize) -> Self {
        Self {
            creds: HashMap::new(),
            queues: vec![ModelQueue::default(); model_count],
            waiting_room: BinaryHeap::new(),
            cooldown_map: HashMap::new(),
            refreshing: HashSet::new(),
        }
    }

    pub fn add_credential(&mut self, id: CredentialId, resource: R, initial_caps_bits: u64) {
        let initial_caps = ModelCapabilities::from_bits(initial_caps_bits);
        let caps = self.creds.get(&id).map(|c| c.caps).unwrap_or(initial_caps);

        self.creds
            .insert(id, RuntimeCredential::new(resource, caps));
        self.refreshing.remove(&id);

        for (index, queue) in self.queues.iter_mut().enumerate() {
            if caps.supports(index) {
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
    ) -> AssignmentResult<R::Lease> {
        self.process_waiting_room();

        let mut result = AssignmentResult::default();

        let Some(model_index) = self.index_from_mask(model_mask) else {
            return result;
        };

        result.stats = self.stats(model_mask);

        // Evaluate sticky hint first if provided.
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
        }

        // Round-robin from queue.
        while let Some(id) = self
            .queues
            .get_mut(model_index)
            .and_then(ModelQueue::pop_front)
        {
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
    fn check_lease(&self, id: CredentialId, model_index: ModelIndex) -> LeaseStatus<R::Lease> {
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

        if cred.inner.is_expired() {
            return LeaseStatus::Expired;
        }

        LeaseStatus::Ready(cred.inner.make_lease(id))
    }

    pub fn report_rate_limit(&mut self, id: CredentialId, model_mask: u64, cooldown: Duration) {
        match R::COOLDOWN_GRANULARITY {
            CooldownScope::PerModel => {
                let Some(model_index) = self.index_from_mask(model_mask) else {
                    return;
                };
                self.insert_cooldown(id, model_index, cooldown);
            }
            CooldownScope::PerCredential => {
                for index in 0..self.queues.len() {
                    self.insert_cooldown(id, index, cooldown);
                }
            }
        }
    }

    fn insert_cooldown(&mut self, id: CredentialId, model_index: ModelIndex, cooldown: Duration) {
        let deadline = Instant::now() + cooldown;
        self.cooldown_map.insert((id, model_index), deadline);
        self.waiting_room
            .push(CooldownTicket(Reverse(deadline), id, model_index));
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

    /// Returns a reference to the inner resource for the given credential.
    pub fn get_credential(&self, id: CredentialId) -> Option<&R> {
        self.creds.get(&id).map(|c| &c.inner)
    }

    /// Returns a clone of the inner resource for the given credential.
    pub fn get_credential_clone(&self, id: CredentialId) -> Option<R> {
        self.creds.get(&id).map(|c| c.inner.clone())
    }

    pub fn contains(&self, id: CredentialId) -> bool {
        self.creds.contains_key(&id)
    }

    pub fn is_refreshing(&self, id: CredentialId) -> bool {
        self.refreshing.contains(&id)
    }

    pub fn total_creds(&self) -> usize {
        self.creds.len()
    }

    pub fn stats(&self, model_mask: u64) -> SchedulerStats {
        let queue_len = self
            .index_from_mask(model_mask)
            .and_then(|i| self.queues.get(i).map(ModelQueue::len))
            .unwrap_or(0);

        SchedulerStats {
            total_creds: self.creds.len(),
            queue_len,
            refreshing: self.refreshing.len(),
            cooldowns: self.cooldown_map.len(),
            ..Default::default()
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
                    if let Some(target_queue) = self.queues.get_mut(reclaimed_model_index) {
                        target_queue.push_back(reclaimed_cred_id);
                    }
                }
                _ => {}
            }
        }
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
    use std::fmt;

    // ── Mock resources ──────────────────────────────────────────────

    #[derive(Debug, Clone)]
    struct MockLease(CredentialId);

    impl LeaseLabel for MockLease {
        fn fmt_label(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "id={}", self.0)
        }
    }

    /// Default mock: PerModel cooldown.
    #[derive(Debug, Clone)]
    struct MockResource(bool);

    impl Schedulable for MockResource {
        type Lease = MockLease;

        fn is_expired(&self) -> bool {
            self.0
        }

        fn make_lease(&self, id: CredentialId) -> MockLease {
            MockLease(id)
        }
    }

    /// PerCredential cooldown variant.
    #[derive(Debug, Clone)]
    struct MockPerCredResource(bool);

    impl Schedulable for MockPerCredResource {
        type Lease = MockLease;
        const COOLDOWN_GRANULARITY: CooldownScope = CooldownScope::PerCredential;

        fn is_expired(&self) -> bool {
            self.0
        }

        fn make_lease(&self, id: CredentialId) -> MockLease {
            MockLease(id)
        }
    }

    type Mgr = CredentialManager<MockResource>;

    fn mask(index: usize) -> u64 {
        1u64 << index
    }

    fn all_caps() -> u64 {
        ModelCapabilities::all().bits()
    }

    fn caps_for(indices: &[usize]) -> u64 {
        let mut c = ModelCapabilities::none();
        for &i in indices {
            c.enable(i);
        }
        c.bits()
    }

    // ── Core scheduling ─────────────────────────────────────────────

    #[test]
    fn add_credential_respects_capabilities() {
        let mut mgr = Mgr::new(2);
        mgr.add_credential(1, MockResource(false), caps_for(&[0]));

        assert!(mgr.get_assigned(mask(0), None).assigned.is_some());
        assert!(mgr.get_assigned(mask(1), None).assigned.is_none());
    }

    #[test]
    fn multiple_credentials_rotate_in_queue() {
        let mut mgr = Mgr::new(1);
        mgr.add_credential(1, MockResource(false), caps_for(&[0]));
        mgr.add_credential(2, MockResource(false), caps_for(&[0]));

        let first = mgr.get_assigned(mask(0), None).assigned.unwrap();
        let second = mgr.get_assigned(mask(0), None).assigned.unwrap();
        assert_eq!(first.0, 1);
        assert_eq!(second.0, 2);
    }

    #[test]
    fn mark_model_unsupported_disables_capability() {
        let mut mgr = Mgr::new(2);
        mgr.add_credential(1, MockResource(false), all_caps());
        mgr.mark_model_unsupported(1, mask(1));

        assert!(mgr.get_assigned(mask(1), None).assigned.is_none());
        assert!(mgr.get_assigned(mask(0), None).assigned.is_some());
    }

    #[test]
    fn readd_after_refresh_preserves_disabled_caps() {
        let mut mgr = Mgr::new(2);
        mgr.add_credential(1, MockResource(false), all_caps());
        mgr.mark_model_unsupported(1, mask(1));

        // re-add with full caps — disabled bit should stick
        mgr.add_credential(1, MockResource(false), all_caps());

        assert!(mgr.get_assigned(mask(1), None).assigned.is_none());
        assert_eq!(mgr.get_assigned(mask(0), None).assigned.unwrap().0, 1);
    }

    // ── Expiry & refresh ────────────────────────────────────────────

    #[test]
    fn expired_token_triggers_refresh_request() {
        let mut mgr = Mgr::new(1);
        mgr.add_credential(1, MockResource(true), caps_for(&[0]));

        let result = mgr.get_assigned(mask(0), None);
        assert!(result.assigned.is_none());
        assert_eq!(result.refresh_ids, vec![1]);
    }

    #[test]
    fn refreshing_credential_is_skipped() {
        let mut mgr = Mgr::new(1);
        mgr.add_credential(1, MockResource(false), caps_for(&[0]));
        mgr.add_credential(2, MockResource(false), caps_for(&[0]));
        mgr.mark_refreshing(1);

        assert_eq!(mgr.get_assigned(mask(0), None).assigned.unwrap().0, 2);
    }

    // ── PerModel cooldown ─────────────────────────────────────────

    #[test]
    fn cooldown_blocks_and_requeues() {
        let mut mgr = Mgr::new(1);
        mgr.add_credential(1, MockResource(false), caps_for(&[0]));

        mgr.report_rate_limit(1, mask(0), Duration::from_millis(10));
        assert!(mgr.get_assigned(mask(0), None).assigned.is_none());

        std::thread::sleep(Duration::from_millis(20));
        assert!(mgr.get_assigned(mask(0), None).assigned.is_some());
    }

    #[test]
    fn model_level_cooldown_only_affects_triggered_model() {
        let mut mgr = Mgr::new(2);
        mgr.add_credential(1, MockResource(false), caps_for(&[0, 1]));

        mgr.report_rate_limit(1, mask(0), Duration::from_secs(60));

        assert!(mgr.get_assigned(mask(0), None).assigned.is_none());
        assert!(mgr.get_assigned(mask(1), None).assigned.is_some());
    }

    #[test]
    fn cooldown_requeue_does_not_duplicate() {
        let mut mgr = Mgr::new(1);
        mgr.add_credential(1, MockResource(false), caps_for(&[0]));

        // pop + push_back (credential stays in queue)
        assert!(mgr.get_assigned(mask(0), None).assigned.is_some());

        mgr.report_rate_limit(1, mask(0), Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(20));

        let result = mgr.get_assigned(mask(0), None);
        assert_eq!(result.stats.queue_len, 1, "credential duplicated in queue");
    }

    // ── PerCredential cooldown ────────────────────────────────────

    type PerCredMgr = CredentialManager<MockPerCredResource>;

    #[test]
    fn credential_level_cooldown_blocks_all_models() {
        let mut mgr = PerCredMgr::new(3);
        mgr.add_credential(1, MockPerCredResource(false), caps_for(&[0, 1, 2]));

        mgr.report_rate_limit(1, mask(0), Duration::from_secs(60));

        assert!(mgr.get_assigned(mask(0), None).assigned.is_none());
        assert!(mgr.get_assigned(mask(1), None).assigned.is_none());
        assert!(mgr.get_assigned(mask(2), None).assigned.is_none());
    }

    #[test]
    fn credential_level_cooldown_recovers_all_models() {
        let mut mgr = PerCredMgr::new(2);
        mgr.add_credential(1, MockPerCredResource(false), caps_for(&[0, 1]));

        mgr.report_rate_limit(1, mask(0), Duration::from_millis(10));
        assert!(mgr.get_assigned(mask(0), None).assigned.is_none());
        assert!(mgr.get_assigned(mask(1), None).assigned.is_none());

        std::thread::sleep(Duration::from_millis(20));
        assert!(mgr.get_assigned(mask(0), None).assigned.is_some());
        assert!(mgr.get_assigned(mask(1), None).assigned.is_some());
    }

    #[test]
    fn credential_level_cooldown_does_not_affect_other_credentials() {
        let mut mgr = PerCredMgr::new(1);
        mgr.add_credential(1, MockPerCredResource(false), caps_for(&[0]));
        mgr.add_credential(2, MockPerCredResource(false), caps_for(&[0]));

        mgr.report_rate_limit(1, mask(0), Duration::from_secs(60));
        assert_eq!(mgr.get_assigned(mask(0), None).assigned.unwrap().0, 2);
    }

    // ── Sticky / route-hit ──────────────────────────────────────────

    #[test]
    fn sticky_valid_credential_returns_route_hit() {
        let mut mgr = Mgr::new(1);
        mgr.add_credential(1, MockResource(false), caps_for(&[0]));

        let result = mgr.get_assigned(mask(0), Some(1));
        assert!(result.route_hit);
        assert_eq!(result.assigned.unwrap().0, 1);
    }

    #[test]
    fn sticky_expired_triggers_refresh_and_falls_back() {
        let mut mgr = Mgr::new(1);
        mgr.add_credential(1, MockResource(true), caps_for(&[0]));
        mgr.add_credential(2, MockResource(false), caps_for(&[0]));

        let result = mgr.get_assigned(mask(0), Some(1));
        assert!(!result.route_hit);
        assert!(result.refresh_ids.contains(&1));
        assert_eq!(result.assigned.unwrap().0, 2);
    }

    #[test]
    fn sticky_cooling_falls_back_to_queue() {
        let mut mgr = Mgr::new(1);
        mgr.add_credential(1, MockResource(false), caps_for(&[0]));
        mgr.add_credential(2, MockResource(false), caps_for(&[0]));

        mgr.report_rate_limit(1, mask(0), Duration::from_secs(60));

        let result = mgr.get_assigned(mask(0), Some(1));
        assert!(!result.route_hit);
        assert_eq!(result.assigned.unwrap().0, 2);
    }

    #[test]
    fn sticky_refreshing_no_duplicate_refresh() {
        let mut mgr = Mgr::new(1);
        mgr.add_credential(1, MockResource(false), caps_for(&[0]));
        mgr.add_credential(2, MockResource(false), caps_for(&[0]));
        mgr.mark_refreshing(1);

        let result = mgr.get_assigned(mask(0), Some(1));
        assert!(!result.route_hit);
        assert!(!result.refresh_ids.contains(&1));
        assert_eq!(result.assigned.unwrap().0, 2);
    }

    #[test]
    fn sticky_missing_falls_back_to_queue() {
        let mut mgr = Mgr::new(1);
        mgr.add_credential(2, MockResource(false), caps_for(&[0]));

        let result = mgr.get_assigned(mask(0), Some(999));
        assert!(!result.route_hit);
        assert_eq!(result.assigned.unwrap().0, 2);
    }

    #[test]
    fn sticky_unsupported_model_falls_back_to_queue() {
        let mut mgr = Mgr::new(2);
        mgr.add_credential(1, MockResource(false), caps_for(&[0]));
        mgr.add_credential(2, MockResource(false), caps_for(&[1]));

        let result = mgr.get_assigned(mask(1), Some(1));
        assert!(!result.route_hit);
        assert_eq!(result.assigned.unwrap().0, 2);
    }

    // ── Stats ───────────────────────────────────────────────────────

    #[test]
    fn stats_reflects_queue_state() {
        let mut mgr = Mgr::new(2);
        mgr.add_credential(1, MockResource(false), caps_for(&[0, 1]));
        mgr.add_credential(2, MockResource(false), caps_for(&[0]));

        assert_eq!(mgr.stats(mask(0)).queue_len, 2);
        assert_eq!(mgr.stats(mask(0)).total_creds, 2);
        assert_eq!(mgr.stats(mask(1)).queue_len, 1);
    }
}
