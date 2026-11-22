//! `PendingAiResolution` locks and the queue of Freeform Action Prompts waiting for them.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 2. A prompt locks exactly the
//! entities in its Snapshot. A prompt whose Snapshot overlaps a lock, or overlaps an earlier
//! prompt still waiting, waits in submission order, which is the Canonical Order. When it can
//! go ahead, the caller builds a fresh Snapshot and locks again with it.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::entity::EntityId;
use crate::snapshot::Snapshot;

/// Identifies 1 submitted Freeform Action Prompt. Tickets are issued in submission order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PromptTicket(u64);

/// What happened to a prompt asking for its locks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// The prompt holds its locks; call the AI DM now.
    Locked,
    /// The prompt waits; [`ResolutionLocks::release`] reports when to try again.
    Queued,
}

/// The kind of change something wants to make to an entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// Walking or otherwise changing position.
    Movement,
    /// Any other change.
    Other,
}

/// Misuse of a ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LockError {
    /// The ticket is not active or queued.
    #[error("prompt ticket {0:?} is not active or queued")]
    UnknownTicket(PromptTicket),
    /// The ticket was asked to lock again before the queue reported it ready.
    #[error("prompt ticket {0:?} is not ready to lock")]
    NotReady(PromptTicket),
}

/// The lock held by 1 active prompt: its Snapshot's entities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingAiResolution {
    entities: BTreeSet<EntityId>,
    player_characters: BTreeSet<EntityId>,
}

impl PendingAiResolution {
    fn from_snapshot(snapshot: &Snapshot) -> Self {
        let entities = snapshot.entities().iter();
        Self {
            entities: entities.clone().map(|entity| entity.world_id).collect(),
            player_characters: entities
                .filter(|entity| entity.player_character)
                .map(|entity| entity.world_id)
                .collect(),
        }
    }

    /// Every locked entity.
    pub fn entities(&self) -> &BTreeSet<EntityId> {
        &self.entities
    }
}

#[derive(Clone, Debug)]
struct QueuedPrompt {
    pending: PendingAiResolution,
    /// The queue has reported this prompt and it is waiting for a fresh Snapshot.
    ready: bool,
}

/// Every active lock and every waiting prompt on 1 server.
#[derive(Clone, Debug, Default)]
pub struct ResolutionLocks {
    next_ticket: u64,
    active: BTreeMap<PromptTicket, PendingAiResolution>,
    owners: BTreeMap<EntityId, PromptTicket>,
    queued: BTreeMap<PromptTicket, QueuedPrompt>,
}

impl ResolutionLocks {
    /// An empty lock table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Submits a prompt with the Snapshot built for it.
    pub fn submit(&mut self, snapshot: &Snapshot) -> (PromptTicket, Admission) {
        let ticket = PromptTicket(self.next_ticket);
        self.next_ticket += 1;
        let pending = PendingAiResolution::from_snapshot(snapshot);
        let admission = self.lock_or_queue(ticket, pending);
        (ticket, admission)
    }

    /// Locks again with a fresh Snapshot after [`Self::release`] reported the ticket ready.
    ///
    /// # Errors
    ///
    /// Returns [`LockError`] if the ticket is not queued or not ready.
    pub fn relock(
        &mut self,
        ticket: PromptTicket,
        snapshot: &Snapshot,
    ) -> Result<Admission, LockError> {
        match self.queued.get(&ticket) {
            None => return Err(LockError::UnknownTicket(ticket)),
            Some(queued) if !queued.ready => return Err(LockError::NotReady(ticket)),
            Some(_) => {}
        }
        self.queued.remove(&ticket);
        Ok(self.lock_or_queue(ticket, PendingAiResolution::from_snapshot(snapshot)))
    }

    /// Replaces an active prompt's lock with the entities of a fresh Snapshot, for the 1
    /// re-resolution after a referenced character moved. The prompt keeps its ticket, so it
    /// keeps its place in the Canonical Order.
    ///
    /// Returns the new admission, which is [`Admission::Queued`] if the fresh Snapshot overlaps
    /// another lock, and the waiting prompts the dropped entities let go ahead, as
    /// [`Self::release`] does.
    ///
    /// # Errors
    ///
    /// Returns [`LockError::UnknownTicket`] if the ticket is not active.
    pub fn refresh(
        &mut self,
        ticket: PromptTicket,
        snapshot: &Snapshot,
    ) -> Result<(Admission, Vec<PromptTicket>), LockError> {
        let previous = self
            .active
            .remove(&ticket)
            .ok_or(LockError::UnknownTicket(ticket))?;
        for entity in &previous.entities {
            self.owners.remove(entity);
        }
        let admission = self.lock_or_queue(ticket, PendingAiResolution::from_snapshot(snapshot));
        Ok((admission, self.mark_ready()))
    }

    /// Ends a prompt, whether it resolved, failed, or was cancelled while waiting.
    ///
    /// Returns the waiting prompts that can now go ahead, in order. Build a fresh Snapshot
    /// for each and call [`Self::relock`].
    ///
    /// # Errors
    ///
    /// Returns [`LockError::UnknownTicket`] if the ticket is not active or queued.
    pub fn release(&mut self, ticket: PromptTicket) -> Result<Vec<PromptTicket>, LockError> {
        if let Some(pending) = self.active.remove(&ticket) {
            for entity in &pending.entities {
                self.owners.remove(entity);
            }
        } else if self.queued.remove(&ticket).is_none() {
            return Err(LockError::UnknownTicket(ticket));
        }
        Ok(self.mark_ready())
    }

    /// Whether anything may make a change of `kind` to `entity` right now.
    ///
    /// Locked entities accept no changes, except that locked player characters may move.
    pub fn may_change(&self, entity: EntityId, kind: ChangeKind) -> bool {
        let Some(owner) = self.owners.get(&entity) else {
            return true;
        };
        kind == ChangeKind::Movement
            && self
                .active
                .get(owner)
                .is_some_and(|pending| pending.player_characters.contains(&entity))
    }

    /// Whether an entity is held by any active prompt.
    pub fn is_locked(&self, entity: EntityId) -> bool {
        self.owners.contains_key(&entity)
    }

    /// The lock held by an active prompt.
    pub fn active(&self, ticket: PromptTicket) -> Option<&PendingAiResolution> {
        self.active.get(&ticket)
    }

    /// Number of prompts waiting.
    pub fn queued_count(&self) -> usize {
        self.queued.len()
    }

    fn lock_or_queue(&mut self, ticket: PromptTicket, pending: PendingAiResolution) -> Admission {
        let blocked_by_lock = pending
            .entities
            .iter()
            .any(|entity| self.owners.contains_key(entity));
        let blocked_by_earlier = self
            .queued
            .range(..ticket)
            .any(|(_, earlier)| !earlier.pending.entities.is_disjoint(&pending.entities));
        if blocked_by_lock || blocked_by_earlier {
            self.queued.insert(
                ticket,
                QueuedPrompt {
                    pending,
                    ready: false,
                },
            );
            Admission::Queued
        } else {
            for &entity in &pending.entities {
                self.owners.insert(entity, ticket);
            }
            self.active.insert(ticket, pending);
            Admission::Locked
        }
    }

    /// Marks every waiting prompt that no longer overlaps a lock or an earlier waiting prompt.
    fn mark_ready(&mut self) -> Vec<PromptTicket> {
        let mut earlier = BTreeSet::new();
        let mut newly_ready = Vec::new();
        for (&ticket, queued) in &mut self.queued {
            let entities = &queued.pending.entities;
            let free = entities.is_disjoint(&earlier)
                && !entities
                    .iter()
                    .any(|entity| self.owners.contains_key(entity));
            if free && !queued.ready {
                queued.ready = true;
                newly_ready.push(ticket);
            }
            earlier.extend(entities.iter().copied());
        }
        newly_ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::tests::entity;
    use crate::snapshot::{PerceptionSettings, SnapshotEntity};

    /// A Snapshot holding exactly the listed entities, the first being the actor.
    fn snapshot_of(entities: &[SnapshotEntity]) -> Snapshot {
        let (actor, rest) = entities.split_first().expect("at least the actor");
        Snapshot::build(
            actor.clone(),
            rest.iter().cloned(),
            |_| false,
            PerceptionSettings {
                hearing_range_squares: 100,
                max_count: 100,
            },
        )
    }

    fn player(world_id: u64) -> SnapshotEntity {
        SnapshotEntity {
            player_character: true,
            ..entity(world_id, 0, 0)
        }
    }

    #[test]
    fn disjoint_prompts_both_lock() {
        let mut locks = ResolutionLocks::new();
        let (_, first) = locks.submit(&snapshot_of(&[entity(1, 0, 0), entity(2, 1, 0)]));
        let (_, second) = locks.submit(&snapshot_of(&[entity(3, 0, 0)]));
        assert_eq!((first, second), (Admission::Locked, Admission::Locked));
    }

    #[test]
    fn locked_entities_refuse_changes_until_release() {
        let mut locks = ResolutionLocks::new();
        let (ticket, _) = locks.submit(&snapshot_of(&[entity(1, 0, 0), entity(2, 1, 0)]));
        assert!(!locks.may_change(EntityId(2), ChangeKind::Movement));
        assert!(locks.may_change(EntityId(9), ChangeKind::Other));
        locks.release(ticket).expect("active ticket");
        assert!(locks.may_change(EntityId(2), ChangeKind::Other));
    }

    #[test]
    fn locked_player_characters_may_still_move() {
        let mut locks = ResolutionLocks::new();
        locks.submit(&snapshot_of(&[player(1), entity(2, 1, 0)]));
        assert!(locks.may_change(EntityId(1), ChangeKind::Movement));
        assert!(!locks.may_change(EntityId(1), ChangeKind::Other));
    }

    #[test]
    fn overlapping_prompt_waits_then_relocks_with_a_fresh_snapshot() {
        let mut locks = ResolutionLocks::new();
        let (first, _) = locks.submit(&snapshot_of(&[entity(1, 0, 0), entity(2, 1, 0)]));
        let (second, admission) = locks.submit(&snapshot_of(&[entity(3, 0, 0), entity(2, 1, 0)]));
        assert_eq!(admission, Admission::Queued);
        assert_eq!(
            locks.relock(second, &snapshot_of(&[entity(3, 0, 0)])),
            Err(LockError::NotReady(second))
        );

        assert_eq!(locks.release(first), Ok(vec![second]));
        let fresh = snapshot_of(&[entity(3, 0, 0), entity(2, 1, 0)]);
        assert_eq!(locks.relock(second, &fresh), Ok(Admission::Locked));
        assert_eq!(locks.queued_count(), 0);
    }

    #[test]
    fn later_prompt_never_overtakes_an_earlier_overlapping_one() {
        let mut locks = ResolutionLocks::new();
        let (first, _) = locks.submit(&snapshot_of(&[entity(1, 0, 0)]));
        let (second, _) = locks.submit(&snapshot_of(&[
            entity(2, 0, 0),
            entity(1, 1, 0),
            entity(3, 2, 0),
        ]));
        // Overlaps only the waiting second prompt, not any lock, and still waits behind it.
        let (third, admission) = locks.submit(&snapshot_of(&[entity(3, 0, 0)]));
        assert_eq!(admission, Admission::Queued);
        assert_eq!(locks.release(first), Ok(vec![second]));
        locks
            .relock(
                second,
                &snapshot_of(&[entity(2, 0, 0), entity(1, 1, 0), entity(3, 2, 0)]),
            )
            .expect("ready ticket");
        assert_eq!(locks.release(second), Ok(vec![third]));
    }

    #[test]
    fn cancelling_a_waiting_prompt_frees_the_ones_behind_it() {
        let mut locks = ResolutionLocks::new();
        locks.submit(&snapshot_of(&[entity(1, 0, 0)]));
        let (second, _) = locks.submit(&snapshot_of(&[
            entity(2, 0, 0),
            entity(1, 1, 0),
            entity(3, 2, 0),
        ]));
        let (third, _) = locks.submit(&snapshot_of(&[entity(3, 0, 0)]));
        assert_eq!(locks.release(second), Ok(vec![third]));
    }

    #[test]
    fn refresh_swaps_the_lock_and_frees_dropped_entities() {
        let mut locks = ResolutionLocks::new();
        let (first, _) = locks.submit(&snapshot_of(&[player(1), entity(2, 1, 0)]));
        let (second, admission) = locks.submit(&snapshot_of(&[entity(3, 0, 0), entity(2, 1, 0)]));
        assert_eq!(admission, Admission::Queued);

        let fresh = snapshot_of(&[player(1), entity(4, 1, 0)]);
        assert_eq!(
            locks.refresh(first, &fresh),
            Ok((Admission::Locked, vec![second]))
        );
        assert!(locks.is_locked(EntityId(4)));
        assert!(!locks.is_locked(EntityId(2)));
        assert_eq!(
            locks.refresh(second, &fresh),
            Err(LockError::UnknownTicket(second))
        );
    }

    #[test]
    fn unknown_tickets_are_errors() {
        let mut locks = ResolutionLocks::new();
        let (ticket, _) = locks.submit(&snapshot_of(&[entity(1, 0, 0)]));
        locks.release(ticket).expect("active ticket");
        assert_eq!(locks.release(ticket), Err(LockError::UnknownTicket(ticket)));
    }
}
