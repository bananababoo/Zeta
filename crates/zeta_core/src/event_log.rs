//! Event Log records: every applied change together with its cause.
//!
//! Rules: `docs/architecture/persistence.md` Sections 2 and 3. This module defines the record
//! shape and an in-memory log; storing records in PostgreSQL belongs to the server. Records
//! are a stored format: fields and tags are only ever added, never renamed or removed.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ai::AiExchange;
use crate::entity::EntityId;
use crate::protocol::inventory::{ItemStack, ItemVerbRequest};
use crate::resolution::{AiResolution, Branch, CheckRoll};
use crate::snapshot::Snapshot;
use crate::time::UtcTimestamp;

/// Position of a record in the Event Log, from 0, with no gaps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventSequence(pub u64);

/// Identifier of 1 Map Partition in the world, for location indexing and future server
/// partitioning.
///
/// Defined Term: Map Partition (`docs/architecture/persistence.md` Section 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MapPartitionId(pub u64);

/// What caused a change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventCause {
    /// A player's Freeform Action Prompt, resolved by the AI DM.
    FreeformAction(FreeformActionCause),
    /// A player's item verb, resolved by deterministic rules without the AI DM.
    ItemVerb(ItemVerbCause),
}

/// Everything needed to explain and replay 1 item verb.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemVerbCause {
    /// The acting character.
    pub actor: EntityId,
    /// The request as the player sent it.
    pub request: ItemVerbRequest,
}

/// Everything needed to explain and replay 1 resolved Freeform Action Prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreeformActionCause {
    /// The acting character.
    pub actor: EntityId,
    /// What the player typed.
    pub player_text: String,
    /// The Snapshot sent to the AI DM, which maps every local identifier in the response.
    pub snapshot: Snapshot,
    /// Every AI request and response, including attempts that failed validation.
    pub exchanges: Vec<AiExchange>,
    /// The roll, if the resolution had a check.
    pub roll: Option<CheckRoll>,
}

/// The change that was applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedChange {
    /// The validated resolution, when the AI DM resolved the cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<AiResolution>,
    /// The branch the roll selected, when the AI DM resolved the cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<Branch>,
    /// Identifiers given to entities created by `spawn` effects or dropped items, in order.
    pub spawned: Vec<EntityId>,
    /// Items that changed hands or equipped state, in order.
    #[serde(default)]
    pub items: Vec<ItemChange>,
}

impl AppliedChange {
    /// The change made by 1 AI resolution branch, before item changes are added.
    pub fn resolved(resolution: AiResolution, branch: Branch, spawned: Vec<EntityId>) -> Self {
        Self {
            resolution: Some(resolution),
            branch: Some(branch),
            spawned,
            items: Vec::new(),
        }
    }
}

/// 1 item change on 1 entity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemChange {
    /// The entity whose inventory changed.
    pub holder: EntityId,
    /// What happened.
    pub kind: ItemChangeKind,
    /// The items involved, with the stack id they had in `holder`'s inventory.
    pub stack: ItemStack,
}

/// What happened to the items in an [`ItemChange`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemChangeKind {
    /// The holder received the items.
    Gained,
    /// The holder lost the items: dropped, given, consumed, or taken.
    Lost,
    /// The holder equipped the stack.
    Equipped,
    /// The holder unequipped the stack.
    Unequipped,
}

/// 1 entry in the Event Log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    /// Position in the log.
    pub sequence: EventSequence,
    /// When the change was applied. Never earlier than the record before it.
    pub recorded_at: UtcTimestamp,
    /// Why the change happened.
    pub cause: EventCause,
    /// What changed.
    pub change: AppliedChange,
}

/// A record that would break the Event Log's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EventLogError {
    /// Real-world time decides the Canonical Order, so time never runs backward in the log.
    #[error("record at {attempted:?} is earlier than the latest record at {latest:?}")]
    OutOfOrder {
        /// Time of the rejected record.
        attempted: UtcTimestamp,
        /// Time of the latest record.
        latest: UtcTimestamp,
    },
}

/// An append-only Event Log held in memory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventLog {
    records: Vec<EventRecord>,
}

impl EventLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a record and returns its sequence number.
    ///
    /// # Errors
    ///
    /// Returns [`EventLogError::OutOfOrder`] if `recorded_at` is earlier than the latest record.
    pub fn append(
        &mut self,
        recorded_at: UtcTimestamp,
        cause: EventCause,
        change: AppliedChange,
    ) -> Result<EventSequence, EventLogError> {
        if let Some(latest) = self.records.last()
            && recorded_at < latest.recorded_at
        {
            return Err(EventLogError::OutOfOrder {
                attempted: recorded_at,
                latest: latest.recorded_at,
            });
        }
        let sequence = EventSequence(self.records.len() as u64);
        self.records.push(EventRecord {
            sequence,
            recorded_at,
            cause,
            change,
        });
        Ok(sequence)
    }

    /// Every record, oldest first.
    pub fn records(&self) -> &[EventRecord] {
        &self.records
    }

    /// Records from `sequence` onward.
    pub fn since(&self, sequence: EventSequence) -> &[EventRecord] {
        let start = usize::try_from(sequence.0).unwrap_or(usize::MAX);
        self.records.get(start..).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::PerceptionSettings;
    use crate::snapshot::tests::entity;

    fn cause() -> EventCause {
        EventCause::FreeformAction(FreeformActionCause {
            actor: EntityId(1),
            player_text: "I light the cart.".into(),
            snapshot: Snapshot::build(
                entity(1, 0, 0),
                [],
                |_| false,
                PerceptionSettings::default(),
            ),
            exchanges: vec![],
            roll: None,
        })
    }

    fn change() -> AppliedChange {
        AppliedChange::resolved(
            AiResolution {
                ok: true,
                chk: None,
                fx: vec![],
                fail: vec![],
            },
            Branch::Success,
            vec![],
        )
    }

    #[test]
    fn records_without_item_fields_still_parse() {
        let old = r#"{"resolution": {"ok": true, "chk": null, "fx": [], "fail": []}, "branch": "success", "spawned": []}"#;
        let parsed: AppliedChange = serde_json::from_str(old).expect("parses");
        assert_eq!(parsed, change());
    }

    #[test]
    fn item_verb_records_round_trip_without_a_resolution() {
        use crate::catalog::CatalogId;
        use crate::protocol::inventory::{Inventory, Item, ItemVerb, StackId};

        let mut inventory = Inventory::default();
        let stack = inventory
            .add(
                Item::Catalog(CatalogId::new("rations").expect("valid id")),
                1,
            )
            .expect("adds");
        let eaten = inventory.remove(stack, 1).expect("removes");
        let cause = EventCause::ItemVerb(ItemVerbCause {
            actor: EntityId(1),
            request: ItemVerbRequest {
                verb: ItemVerb::Consume,
                stack: StackId(0),
                quantity: 1,
                recipient: None,
                craft_target: None,
            },
        });
        let change = AppliedChange {
            resolution: None,
            branch: None,
            spawned: vec![],
            items: vec![ItemChange {
                holder: EntityId(1),
                kind: ItemChangeKind::Lost,
                stack: eaten,
            }],
        };
        let mut log = EventLog::new();
        log.append(UtcTimestamp::from_unix_milliseconds(1), cause, change)
            .expect("first record");
        let json = serde_json::to_value(&log.records()[0]).expect("serializes");
        assert_eq!(json["cause"]["kind"], "item_verb");
        assert!(json["change"].get("resolution").is_none());
        let parsed: EventRecord = serde_json::from_value(json).expect("parses");
        assert_eq!(parsed, log.records()[0]);
    }

    #[test]
    fn sequence_numbers_count_up_from_zero() {
        let mut log = EventLog::new();
        let at = UtcTimestamp::from_unix_milliseconds(100);
        assert_eq!(log.append(at, cause(), change()), Ok(EventSequence(0)));
        assert_eq!(log.append(at, cause(), change()), Ok(EventSequence(1)));
        assert_eq!(log.since(EventSequence(1)).len(), 1);
        assert_eq!(log.since(EventSequence(5)), []);
    }

    #[test]
    fn records_earlier_than_the_latest_are_rejected() {
        let mut log = EventLog::new();
        let later = UtcTimestamp::from_unix_milliseconds(200);
        let earlier = UtcTimestamp::from_unix_milliseconds(100);
        log.append(later, cause(), change()).expect("first record");
        assert_eq!(
            log.append(earlier, cause(), change()),
            Err(EventLogError::OutOfOrder {
                attempted: earlier,
                latest: later
            })
        );
    }

    #[test]
    fn records_round_trip_through_json_with_a_cause_tag() {
        let mut log = EventLog::new();
        log.append(UtcTimestamp::from_unix_milliseconds(1), cause(), change())
            .expect("first record");
        let json = serde_json::to_value(&log.records()[0]).expect("serializes");
        assert_eq!(json["cause"]["kind"], "freeform_action");
        let parsed: EventRecord = serde_json::from_value(json).expect("parses");
        assert_eq!(parsed, log.records()[0]);
    }
}
