//! The AI Resolution schema the AI DM returns, and how a dice roll picks its branch.
//!
//! The types match the AI Resolution Schema in `docs/architecture/ai_orchestration.md`
//! exactly, and the field names and `op` tags are a stored wire format: never rename them.
//! Dice rules: Section 3 of the same document.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::catalog::DamageType;
use crate::protocol::inventory::CustomItem;
use crate::protocol::properties::{
    ActiveEffect, ClassId, ConditionId, CreatureType, HazardId, ResourceTarget, Size, SubclassId,
};

/// Local integer identifier assigned per request in the Snapshot.
pub type LocalId = u32;

/// The AI DM's complete answer to 1 Freeform Action Prompt.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AiResolution {
    /// true = realistic, false = impossible
    pub ok: bool,
    /// Difficulty Class check, or None if the action is automatic
    pub chk: Option<Check>,
    /// Effects executed on success (or if chk is None)
    pub fx: Vec<WorldEffect>,
    /// Effects executed on failure (empty if chk is None)
    pub fail: Vec<WorldEffect>,
}

/// A Difficulty Class check the acting character must roll against.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// Target Difficulty Class, set before any roll exists.
    pub dc: u8,
    /// Why the action needs a check, shown to players.
    pub reason: String,
}

/// How loudly an entity speaks, which decides who hears it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Volume {
    /// Heard only nearby.
    Whisper,
    /// Ordinary speech.
    Normal,
    /// Heard far away.
    Shout,
}

/// Final state an entity settles into after an unstable process ends.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Settle {
    /// In-game minutes until the entity settles
    pub minutes: u32,
    /// Tags added when the entity settles.
    #[serde(default)]
    pub add: Vec<String>,
    /// Tags removed when the entity settles.
    #[serde(default)]
    pub remove: Vec<String>,
    /// New passable value, or None for no change.
    pub passable: Option<bool>,
    /// New visual description, or None for no change.
    pub visual: Option<String>,
}

/// Optional 2024 5e mechanical attributes attached to an AI-spawned entity.
///
/// Leaving `props` as `None` (or individual fields as `None`/empty) omits those components.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct SpawnProps {
    /// Maximum and starting hit points (`Health::full`). Must be at least 1 when set.
    #[serde(default)]
    pub hp: Option<u16>,
    /// Base Armor Class (`ArmorClass`). Must be at least 1 when set.
    #[serde(default)]
    pub ac: Option<u8>,
    /// Optional structural damage threshold (`ArmorClass::damage_threshold`).
    #[serde(default)]
    pub damage_threshold: Option<u16>,
    /// Optional 2024 5e size category (`Size`).
    #[serde(default)]
    pub size: Option<Size>,
    /// Optional 2024 5e creature type (`CreatureType`). Also marks the entity as a `Creature`.
    #[serde(default)]
    pub creature_type: Option<CreatureType>,
    /// Optional walking speed in feet (`Speed::walk_only`).
    #[serde(default)]
    pub walk_speed: Option<u16>,
    /// Optional damage resistances (`DamageAffinities::resistances`).
    #[serde(default)]
    pub resistances: Vec<DamageType>,
    /// Optional damage immunities (`DamageAffinities::immunities`).
    #[serde(default)]
    pub immunities: Vec<DamageType>,
    /// Optional damage vulnerabilities (`DamageAffinities::vulnerabilities`).
    #[serde(default)]
    pub vulnerabilities: Vec<DamageType>,
}

/// 1 change to the world. Every field naming an entity holds a Snapshot [`LocalId`].
#[allow(
    missing_docs,
    reason = "each variant documents its fields with its JSON example"
)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorldEffect {
    /// `{"op": "narrate", "text": "The rope snaps."}`
    Narrate { text: String },
    /// `{"op": "speak", "id": 3, "text": "Halt!", "vol": "shout"}`
    Speak {
        id: LocalId,
        text: String,
        vol: Volume,
    },
    /// `{"op": "move", "id": 3, "x": 12, "y": 7, "style": "stumbles"}`
    Move {
        id: LocalId,
        x: u32,
        y: u32,
        style: String,
    },
    /// `{"op": "modify", "id": 5, "add": ["burning"], "remove": ["dry"], "passable": false, "visual": "a flaming cart", "settle": {...}}`
    Modify {
        id: LocalId,
        #[serde(default)]
        add: Vec<String>,
        #[serde(default)]
        remove: Vec<String>,
        passable: Option<bool>,
        visual: Option<String>,
        settle: Option<Settle>,
    },
    /// `{"op": "spawn", "name": "Smoke", "x": 12, "y": 8, "passable": true, "tags": ["obscuring"], "visual": "thick grey smoke", "settle": {...}, "props": null}`
    Spawn {
        name: String,
        x: u32,
        y: u32,
        passable: bool,
        tags: Vec<String>,
        visual: String,
        settle: Option<Settle>,
        #[serde(default)]
        props: Option<SpawnProps>,
    },
    /// `{"op": "destroy", "id": 5, "reason": "burned to ash"}`
    Destroy { id: LocalId, reason: String },
    /// `{"op": "history", "ids": [3, 5], "summary": "The guard watched the cart burn.", "preserved": true}`
    History {
        ids: Vec<LocalId>,
        summary: String,
        preserved: bool,
    },
    /// `{"op": "bubble", "active": true, "ids": [0, 3]}`
    Bubble { active: bool, ids: Vec<LocalId> },
    /// `{"op": "award", "id": 0, "item": "rope", "custom": null, "qty": 1}`: gives entity `id`
    /// `qty` of 1 catalog item, or of 1 custom narrative item when `item` is null.
    Award {
        id: LocalId,
        item: Option<String>,
        custom: Option<CustomItem>,
        qty: u32,
    },
    /// `{"op": "take", "id": 3, "item": "rope", "custom_name": null, "qty": 1}`: removes up to
    /// `qty` of 1 catalog item, or of the custom item named `custom_name`, from entity `id`.
    Take {
        id: LocalId,
        item: Option<String>,
        custom_name: Option<String>,
        qty: u32,
    },
    /// `{"op": "place_item", "from": 0, "x": 14, "y": 20, "item": "longsword", "custom": null, "qty": 1}`:
    /// moves an item from `from` onto a ground pile at `(x, y)`, or spawns ground loot when
    /// `from` is `None`.
    PlaceItem {
        from: Option<LocalId>,
        x: u32,
        y: u32,
        item: Option<String>,
        custom: Option<CustomItem>,
        qty: u32,
    },
    /// `{"op": "damage", "id": 3, "amount": 8, "damage_type": "fire"}`: deals raw typed damage.
    Damage {
        id: LocalId,
        amount: u16,
        damage_type: DamageType,
    },
    /// `{"op": "heal", "id": 0, "amount": 6}`: restores up to `amount` hit points.
    Heal { id: LocalId, amount: u16 },
    /// `{"op": "temp_hp", "id": 0, "amount": 5}`: sets temporary hit points (`max(existing, amount)`).
    TempHp { id: LocalId, amount: u16 },
    /// `{"op": "condition", "id": 3, "add": ["prone"], "remove": [], "exhaustion_delta": 0, "add_hazards": [], "remove_hazards": []}`
    Condition {
        id: LocalId,
        #[serde(default)]
        add: Vec<ConditionId>,
        #[serde(default)]
        remove: Vec<ConditionId>,
        #[serde(default)]
        exhaustion_delta: i8,
        #[serde(default)]
        add_hazards: Vec<HazardId>,
        #[serde(default)]
        remove_hazards: Vec<HazardId>,
    },
    /// `{"op": "apply_effect", "id": 0, "effect": {...}, "concentrate": true}`: attaches a buff/debuff.
    ApplyEffect {
        id: LocalId,
        effect: ActiveEffect,
        #[serde(default)]
        concentrate: bool,
    },
    /// `{"op": "remove_effect", "id": 0, "effect_id": "bless"}`: removes an active effect by id.
    RemoveEffect { id: LocalId, effect_id: String },
    /// `{"op": "spend_resource", "id": 0, "target": {...}, "amount": 1}`: spends from a pool.
    SpendResource {
        id: LocalId,
        target: ResourceTarget,
        amount: u16,
    },
    /// `{"op": "restore_resource", "id": 0, "target": {...}, "amount": 1}`: restores to a pool.
    RestoreResource {
        id: LocalId,
        target: ResourceTarget,
        amount: u16,
    },
    /// `{"op": "advance_level", "id": 0, "class": "fighter", "subclass": null}`: adds 1 class level.
    AdvanceLevel {
        id: LocalId,
        class: ClassId,
        #[serde(default)]
        subclass: Option<SubclassId>,
    },
}

impl WorldEffect {
    /// The wire name of this effect's `op` tag, used in validation messages.
    pub const fn op(&self) -> &'static str {
        match self {
            Self::Narrate { .. } => "narrate",
            Self::Speak { .. } => "speak",
            Self::Move { .. } => "move",
            Self::Modify { .. } => "modify",
            Self::Spawn { .. } => "spawn",
            Self::Destroy { .. } => "destroy",
            Self::History { .. } => "history",
            Self::Bubble { .. } => "bubble",
            Self::Award { .. } => "award",
            Self::Take { .. } => "take",
            Self::PlaceItem { .. } => "place_item",
            Self::Damage { .. } => "damage",
            Self::Heal { .. } => "heal",
            Self::TempHp { .. } => "temp_hp",
            Self::Condition { .. } => "condition",
            Self::ApplyEffect { .. } => "apply_effect",
            Self::RemoveEffect { .. } => "remove_effect",
            Self::SpendResource { .. } => "spend_resource",
            Self::RestoreResource { .. } => "restore_resource",
            Self::AdvanceLevel { .. } => "advance_level",
        }
    }

    /// Every Snapshot identifier this effect references, in field order.
    pub fn referenced_ids(&self) -> &[LocalId] {
        match self {
            Self::Speak { id, .. }
            | Self::Move { id, .. }
            | Self::Modify { id, .. }
            | Self::Destroy { id, .. }
            | Self::Award { id, .. }
            | Self::Take { id, .. }
            | Self::Damage { id, .. }
            | Self::Heal { id, .. }
            | Self::TempHp { id, .. }
            | Self::Condition { id, .. }
            | Self::ApplyEffect { id, .. }
            | Self::RemoveEffect { id, .. }
            | Self::SpendResource { id, .. }
            | Self::RestoreResource { id, .. }
            | Self::AdvanceLevel { id, .. } => std::slice::from_ref(id),
            Self::History { ids, .. } | Self::Bubble { ids, .. } => ids,
            Self::PlaceItem { from, .. } => from.as_slice(),
            Self::Narrate { .. } | Self::Spawn { .. } => &[],
        }
    }

    /// The grid square this effect places something on, if any.
    pub const fn target_square(&self) -> Option<(u32, u32)> {
        match self {
            Self::Move { x, y, .. } | Self::Spawn { x, y, .. } | Self::PlaceItem { x, y, .. } => {
                Some((*x, *y))
            }
            _ => None,
        }
    }
}

/// The natural result of 1 d20, from 1 to 20.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct D20(u8);

/// A number that cannot be the face of a d20.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("{0} is not a d20 face; faces run from 1 to 20")]
pub struct InvalidD20(pub u8);

impl D20 {
    /// Wraps a die face.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidD20`] unless `face` is between 1 and 20.
    pub const fn new(face: u8) -> Result<Self, InvalidD20> {
        if face >= 1 && face <= 20 {
            Ok(Self(face))
        } else {
            Err(InvalidD20(face))
        }
    }

    /// The face rolled.
    pub const fn face(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for D20 {
    type Error = InvalidD20;

    fn try_from(face: u8) -> Result<Self, Self::Error> {
        Self::new(face)
    }
}

impl From<D20> for u8 {
    fn from(die: D20) -> Self {
        die.0
    }
}

/// A rolled check: the visible d20 plus the character's modifier.
///
/// The modifier comes from the character, never from the AI DM.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRoll {
    /// The d20 rolled on screen.
    pub natural: D20,
    /// The character's bonus or penalty for this check.
    pub modifier: i8,
}

impl CheckRoll {
    /// The natural roll plus the modifier.
    pub fn total(self) -> i16 {
        i16::from(self.natural.0) + i16::from(self.modifier)
    }

    /// Whether the total meets or beats a Difficulty Class.
    pub fn meets(self, dc: u8) -> bool {
        self.total() >= i16::from(dc)
    }
}

/// Which list of effects in an [`AiResolution`] applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Branch {
    /// The `fx` list.
    Success,
    /// The `fail` list.
    Failure,
}

/// A roll that does not fit the resolution it is applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BranchError {
    /// The resolution has a check but no roll was supplied.
    #[error("the resolution has a check, so a roll is required")]
    RollMissing,
    /// The resolution has no check but a roll was supplied.
    #[error("the resolution has no check, so no roll may be applied")]
    UnexpectedRoll,
}

impl AiResolution {
    /// Picks the branch to apply.
    ///
    /// # Errors
    ///
    /// Returns [`BranchError`] when `roll` is present without a check, or missing with one.
    pub fn branch(&self, roll: Option<CheckRoll>) -> Result<Branch, BranchError> {
        match (&self.chk, roll) {
            (None, None) => Ok(Branch::Success),
            (None, Some(_)) => Err(BranchError::UnexpectedRoll),
            (Some(_), None) => Err(BranchError::RollMissing),
            (Some(check), Some(roll)) if roll.meets(check.dc) => Ok(Branch::Success),
            (Some(_), Some(_)) => Ok(Branch::Failure),
        }
    }

    /// The effects in a branch.
    pub fn effects(&self, branch: Branch) -> &[WorldEffect] {
        match branch {
            Branch::Success => &self.fx,
            Branch::Failure => &self.fail,
        }
    }

    /// JSON Schema for [`AiResolution`], matching the AI Resolution Schema wire format.
    pub fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "ok": { "type": "boolean" },
                "chk": {
                    "anyOf": [
                        { "$ref": "#/$defs/Check" },
                        { "type": "null" }
                    ]
                },
                "fx": {
                    "type": "array",
                    "items": { "$ref": "#/$defs/WorldEffect" }
                },
                "fail": {
                    "type": "array",
                    "items": { "$ref": "#/$defs/WorldEffect" }
                }
            },
            "required": ["ok", "chk", "fx", "fail"],
            "$defs": {
                "Check": object_schema(
                    &serde_json::json!({
                        "dc": { "type": "integer" },
                        "reason": { "type": "string" }
                    }),
                    &["dc", "reason"],
                ),
                "Volume": {
                    "type": "string",
                    "enum": ["whisper", "normal", "shout"]
                },
                "Dice": object_schema(
                    &serde_json::json!({
                        "count": { "type": "integer" },
                        "sides": { "type": "integer" }
                    }),
                    &["count", "sides"],
                ),
                "StatModifier": stat_modifier_json_schema(),
                "ActiveEffect": active_effect_json_schema(),
                "ResourceTarget": resource_target_json_schema(),
                "CustomItem": custom_item_json_schema(),
                "SpawnProps": spawn_props_json_schema(),
                "Settle": settle_json_schema(),
                "WorldEffect": world_effect_json_schema(),
            }
        })
    }
}

fn nullable_ref(def: &str) -> serde_json::Value {
    serde_json::json!({
        "anyOf": [
            { "$ref": format!("#/$defs/{def}") },
            { "type": "null" }
        ]
    })
}

/// Every object in the wire JSON Schema marks all of its properties `required` (using
/// nullable types instead of omitted keys) and leaves large closed enums as plain strings
/// validated by Rust `serde` + `validation.rs`. Otherwise Gemini's structured-output
/// finite-state grammar hits its serving state limit (HTTP 400).
fn stat_modifier_json_schema() -> serde_json::Value {
    object_schema(
        &serde_json::json!({
            "kind": { "type": "string" },
            "value": { "type": "integer" },
            "dice": nullable_ref("Dice"),
            "dice_negative": { "type": "boolean" },
            "roll_scope": { "type": ["string", "null"] },
            "ability": { "type": ["string", "null"] },
            "skill": { "type": ["string", "null"] },
            "damage_type": { "type": ["string", "null"] }
        }),
        &[
            "kind",
            "value",
            "dice",
            "dice_negative",
            "roll_scope",
            "ability",
            "skill",
            "damage_type",
        ],
    )
}

fn active_effect_json_schema() -> serde_json::Value {
    object_schema(
        &serde_json::json!({
            "id": { "type": "string" },
            "name": { "type": "string" },
            "source": { "type": ["string", "null"] },
            "duration_rounds": { "type": ["integer", "null"] },
            "modifiers": {
                "type": "array",
                "items": { "$ref": "#/$defs/StatModifier" }
            }
        }),
        &["id", "name", "source", "duration_rounds", "modifiers"],
    )
}

fn resource_target_json_schema() -> serde_json::Value {
    object_schema(
        &serde_json::json!({
            "kind": { "type": "string" },
            "spell_level": { "type": ["integer", "null"] },
            "resource": { "type": ["string", "null"] },
            "feature_id": { "type": ["string", "null"] },
            "stack_id": { "type": ["integer", "null"] }
        }),
        &["kind", "spell_level", "resource", "feature_id", "stack_id"],
    )
}

fn object_schema(properties: &serde_json::Value, required: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required,
    })
}

fn custom_item_json_schema() -> serde_json::Value {
    object_schema(
        &serde_json::json!({
            "name": { "type": "string" },
            "weight_thousandths_of_pound": { "type": "integer" },
            "description": { "type": "string" },
            "tags": { "type": "array", "items": { "type": "string" } }
        }),
        &["name", "weight_thousandths_of_pound", "description", "tags"],
    )
}

fn spawn_props_json_schema() -> serde_json::Value {
    object_schema(
        &serde_json::json!({
            "hp": { "type": ["integer", "null"] },
            "ac": { "type": ["integer", "null"] },
            "damage_threshold": { "type": ["integer", "null"] },
            "size": { "type": ["string", "null"] },
            "creature_type": { "type": ["string", "null"] },
            "walk_speed": { "type": ["integer", "null"] },
            "resistances": { "type": "array", "items": { "type": "string" } },
            "immunities": { "type": "array", "items": { "type": "string" } },
            "vulnerabilities": { "type": "array", "items": { "type": "string" } }
        }),
        &[
            "hp",
            "ac",
            "damage_threshold",
            "size",
            "creature_type",
            "walk_speed",
            "resistances",
            "immunities",
            "vulnerabilities",
        ],
    )
}

fn settle_json_schema() -> serde_json::Value {
    object_schema(
        &serde_json::json!({
            "minutes": { "type": "integer" },
            "add": { "type": "array", "items": { "type": "string" } },
            "remove": { "type": "array", "items": { "type": "string" } },
            "passable": { "type": ["boolean", "null"] },
            "visual": { "type": ["string", "null"] }
        }),
        &["minutes", "add", "remove", "passable", "visual"],
    )
}

fn world_effect_json_schema() -> serde_json::Value {
    let mut variants = vec![
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["narrate"] },
                "text": { "type": "string" }
            }),
            &["op", "text"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["speak"] },
                "id": { "type": "integer" },
                "text": { "type": "string" },
                "vol": { "$ref": "#/$defs/Volume" }
            }),
            &["op", "id", "text", "vol"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["move"] },
                "id": { "type": "integer" },
                "x": { "type": "integer" },
                "y": { "type": "integer" },
                "style": { "type": "string" }
            }),
            &["op", "id", "x", "y", "style"],
        ),
    ];
    variants.extend(settling_effect_schemas());
    variants.extend([
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["destroy"] },
                "id": { "type": "integer" },
                "reason": { "type": "string" }
            }),
            &["op", "id", "reason"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["history"] },
                "ids": { "type": "array", "items": { "type": "integer" } },
                "summary": { "type": "string" },
                "preserved": { "type": "boolean" }
            }),
            &["op", "ids", "summary", "preserved"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["bubble"] },
                "active": { "type": "boolean" },
                "ids": { "type": "array", "items": { "type": "integer" } }
            }),
            &["op", "active", "ids"],
        ),
    ]);
    variants.extend(item_effect_schemas());
    variants.extend(property_effect_schemas());
    serde_json::json!({ "anyOf": variants })
}

fn property_effect_schemas() -> [serde_json::Value; 9] {
    [
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["damage"] },
                "id": { "type": "integer" },
                "amount": { "type": "integer" },
                "damage_type": { "type": "string" }
            }),
            &["op", "id", "amount", "damage_type"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["heal"] },
                "id": { "type": "integer" },
                "amount": { "type": "integer" }
            }),
            &["op", "id", "amount"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["temp_hp"] },
                "id": { "type": "integer" },
                "amount": { "type": "integer" }
            }),
            &["op", "id", "amount"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["condition"] },
                "id": { "type": "integer" },
                "add": { "type": "array", "items": { "type": "string" } },
                "remove": { "type": "array", "items": { "type": "string" } },
                "exhaustion_delta": { "type": "integer" },
                "add_hazards": { "type": "array", "items": { "type": "string" } },
                "remove_hazards": { "type": "array", "items": { "type": "string" } }
            }),
            &[
                "op",
                "id",
                "add",
                "remove",
                "exhaustion_delta",
                "add_hazards",
                "remove_hazards",
            ],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["apply_effect"] },
                "id": { "type": "integer" },
                "effect": { "$ref": "#/$defs/ActiveEffect" },
                "concentrate": { "type": "boolean" }
            }),
            &["op", "id", "effect", "concentrate"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["remove_effect"] },
                "id": { "type": "integer" },
                "effect_id": { "type": "string" }
            }),
            &["op", "id", "effect_id"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["spend_resource"] },
                "id": { "type": "integer" },
                "target": { "$ref": "#/$defs/ResourceTarget" },
                "amount": { "type": "integer" }
            }),
            &["op", "id", "target", "amount"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["restore_resource"] },
                "id": { "type": "integer" },
                "target": { "$ref": "#/$defs/ResourceTarget" },
                "amount": { "type": "integer" }
            }),
            &["op", "id", "target", "amount"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["advance_level"] },
                "id": { "type": "integer" },
                "class": { "type": "string" },
                "subclass": { "type": ["string", "null"] }
            }),
            &["op", "id", "class", "subclass"],
        ),
    ]
}

fn item_effect_schemas() -> [serde_json::Value; 3] {
    [
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["award"] },
                "id": { "type": "integer" },
                "item": { "type": ["string", "null"] },
                "custom": nullable_ref("CustomItem"),
                "qty": { "type": "integer" }
            }),
            &["op", "id", "item", "custom", "qty"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["take"] },
                "id": { "type": "integer" },
                "item": { "type": ["string", "null"] },
                "custom_name": { "type": ["string", "null"] },
                "qty": { "type": "integer" }
            }),
            &["op", "id", "item", "custom_name", "qty"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["place_item"] },
                "from": { "type": ["integer", "null"] },
                "x": { "type": "integer" },
                "y": { "type": "integer" },
                "item": { "type": ["string", "null"] },
                "custom": nullable_ref("CustomItem"),
                "qty": { "type": "integer" }
            }),
            &["op", "from", "x", "y", "item", "custom", "qty"],
        ),
    ]
}

fn settling_effect_schemas() -> [serde_json::Value; 2] {
    let settle_ref = nullable_ref("Settle");
    [
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["modify"] },
                "id": { "type": "integer" },
                "add": { "type": "array", "items": { "type": "string" } },
                "remove": { "type": "array", "items": { "type": "string" } },
                "passable": { "type": ["boolean", "null"] },
                "visual": { "type": ["string", "null"] },
                "settle": settle_ref.clone()
            }),
            &["op", "id", "add", "remove", "passable", "visual", "settle"],
        ),
        object_schema(
            &serde_json::json!({
                "op": { "type": "string", "enum": ["spawn"] },
                "name": { "type": "string" },
                "x": { "type": "integer" },
                "y": { "type": "integer" },
                "passable": { "type": "boolean" },
                "tags": { "type": "array", "items": { "type": "string" } },
                "visual": { "type": "string" },
                "settle": settle_ref,
                "props": nullable_ref("SpawnProps")
            }),
            &[
                "op", "name", "x", "y", "passable", "tags", "visual", "settle", "props",
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses 1 effect from JSON, failing the test with the parser message.
    fn effect(json: &str) -> WorldEffect {
        serde_json::from_str(json).unwrap_or_else(|error| panic!("{json}: {error}"))
    }

    /// The doc writes `"settle": {...}` as a placeholder; tests use this concrete value.
    const SETTLE: &str = r#"{"minutes": 30, "add": ["ash"], "remove": ["burning"], "passable": true, "visual": "a heap of ash"}"#;

    #[test]
    fn documented_effect_examples_parse() {
        assert_eq!(
            effect(r#"{"op": "narrate", "text": "The rope snaps."}"#),
            WorldEffect::Narrate {
                text: "The rope snaps.".into()
            }
        );
        assert_eq!(
            effect(r#"{"op": "speak", "id": 3, "text": "Halt!", "vol": "shout"}"#),
            WorldEffect::Speak {
                id: 3,
                text: "Halt!".into(),
                vol: Volume::Shout
            }
        );
        assert_eq!(
            effect(r#"{"op": "move", "id": 3, "x": 12, "y": 7, "style": "stumbles"}"#),
            WorldEffect::Move {
                id: 3,
                x: 12,
                y: 7,
                style: "stumbles".into()
            }
        );
        assert_eq!(
            effect(r#"{"op": "destroy", "id": 5, "reason": "burned to ash"}"#),
            WorldEffect::Destroy {
                id: 5,
                reason: "burned to ash".into()
            }
        );
        assert_eq!(
            effect(
                r#"{"op": "history", "ids": [3, 5], "summary": "The guard watched the cart burn.", "preserved": true}"#
            ),
            WorldEffect::History {
                ids: vec![3, 5],
                summary: "The guard watched the cart burn.".into(),
                preserved: true,
            }
        );
        assert_eq!(
            effect(r#"{"op": "bubble", "active": true, "ids": [0, 3]}"#),
            WorldEffect::Bubble {
                active: true,
                ids: vec![0, 3]
            }
        );
    }

    #[test]
    fn documented_settling_examples_parse() {
        let settle = Settle {
            minutes: 30,
            add: vec!["ash".into()],
            remove: vec!["burning".into()],
            passable: Some(true),
            visual: Some("a heap of ash".into()),
        };
        let modify = format!(
            r#"{{"op": "modify", "id": 5, "add": ["burning"], "remove": ["dry"], "passable": false, "visual": "a flaming cart", "settle": {SETTLE}}}"#
        );
        assert_eq!(
            effect(&modify),
            WorldEffect::Modify {
                id: 5,
                add: vec!["burning".into()],
                remove: vec!["dry".into()],
                passable: Some(false),
                visual: Some("a flaming cart".into()),
                settle: Some(settle.clone()),
            }
        );
        let spawn = format!(
            r#"{{"op": "spawn", "name": "Smoke", "x": 12, "y": 8, "passable": true, "tags": ["obscuring"], "visual": "thick grey smoke", "settle": {SETTLE}}}"#
        );
        assert_eq!(
            effect(&spawn),
            WorldEffect::Spawn {
                name: "Smoke".into(),
                x: 12,
                y: 8,
                passable: true,
                tags: vec!["obscuring".into()],
                visual: "thick grey smoke".into(),
                settle: Some(settle),
                props: None,
            }
        );
        let barricade = effect(
            r#"{"op": "spawn", "name": "Barricade", "x": 5, "y": 6, "passable": false, "tags": ["wooden"], "visual": "stacked timber", "settle": null, "props": {"hp": 20, "ac": 15, "size": "large", "immunities": ["poison", "psychic"], "vulnerabilities": ["fire"]}}"#,
        );
        assert_eq!(
            barricade,
            WorldEffect::Spawn {
                name: "Barricade".into(),
                x: 5,
                y: 6,
                passable: false,
                tags: vec!["wooden".into()],
                visual: "stacked timber".into(),
                settle: None,
                props: Some(SpawnProps {
                    hp: Some(20),
                    ac: Some(15),
                    damage_threshold: None,
                    size: Some(Size::Large),
                    creature_type: None,
                    walk_speed: None,
                    resistances: vec![],
                    immunities: vec![DamageType::Poison, DamageType::Psychic],
                    vulnerabilities: vec![DamageType::Fire],
                }),
            }
        );
    }

    #[test]
    fn omitted_optional_fields_mean_no_change() {
        assert_eq!(
            effect(r#"{"op": "modify", "id": 1}"#),
            WorldEffect::Modify {
                id: 1,
                add: vec![],
                remove: vec![],
                passable: None,
                visual: None,
                settle: None,
            }
        );
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_level() {
        let misspelled_effect = r#"{"op": "modify", "id": 1, "pasable": false}"#;
        assert!(serde_json::from_str::<WorldEffect>(misspelled_effect).is_err());
        let extra_top_level = r#"{"ok": true, "chk": null, "fx": [], "fail": [], "mood": "tense"}"#;
        assert!(serde_json::from_str::<AiResolution>(extra_top_level).is_err());
        let extra_in_check = r#"{"dc": 10, "reason": "r", "bonus": 2}"#;
        assert!(serde_json::from_str::<Check>(extra_in_check).is_err());
        let extra_in_settle = r#"{"minutes": 1, "color": "red"}"#;
        assert!(serde_json::from_str::<Settle>(extra_in_settle).is_err());
    }

    #[test]
    fn unknown_op_is_rejected() {
        assert!(serde_json::from_str::<WorldEffect>(r#"{"op": "teleport", "id": 1}"#).is_err());
    }

    #[test]
    fn resolution_round_trips_through_json() {
        let resolution = AiResolution {
            ok: true,
            chk: Some(Check {
                dc: 12,
                reason: "slippery rope".into(),
            }),
            fx: vec![effect(r#"{"op": "narrate", "text": "You climb."}"#)],
            fail: vec![effect(r#"{"op": "narrate", "text": "You slip."}"#)],
        };
        let json = serde_json::to_string(&resolution).expect("serializes");
        let parsed: AiResolution = serde_json::from_str(&json).expect("parses");
        assert_eq!(parsed, resolution);
    }

    #[test]
    fn op_names_match_the_wire_tags() {
        let spawn = effect(
            r#"{"op": "spawn", "name": "Smoke", "x": 1, "y": 1, "passable": true, "tags": [], "visual": "smoke", "settle": null}"#,
        );
        let json = serde_json::to_value(&spawn).expect("serializes");
        assert_eq!(json["op"], spawn.op());
    }

    #[test]
    fn d20_rejects_faces_outside_one_to_twenty() {
        assert_eq!(D20::new(0), Err(InvalidD20(0)));
        assert_eq!(D20::new(21), Err(InvalidD20(21)));
        assert!(serde_json::from_str::<D20>("21").is_err());
        assert_eq!(D20::new(20).map(D20::face), Ok(20));
    }

    fn roll(face: u8, modifier: i8) -> CheckRoll {
        CheckRoll {
            natural: D20::new(face).expect("valid face"),
            modifier,
        }
    }

    fn checked(dc: u8) -> AiResolution {
        AiResolution {
            ok: true,
            chk: Some(Check {
                dc,
                reason: "test".into(),
            }),
            fx: vec![],
            fail: vec![],
        }
    }

    #[test]
    fn meeting_the_difficulty_class_succeeds() {
        assert_eq!(checked(12).branch(Some(roll(10, 2))), Ok(Branch::Success));
        assert_eq!(checked(12).branch(Some(roll(10, 1))), Ok(Branch::Failure));
    }

    #[test]
    fn roll_must_match_whether_a_check_exists() {
        let automatic = AiResolution {
            chk: None,
            ..checked(1)
        };
        assert_eq!(automatic.branch(None), Ok(Branch::Success));
        assert_eq!(
            automatic.branch(Some(roll(5, 0))),
            Err(BranchError::UnexpectedRoll)
        );
        assert_eq!(checked(1).branch(None), Err(BranchError::RollMissing));
    }

    #[test]
    fn json_schema_forbids_unknown_properties_and_covers_all_effects() {
        let schema = AiResolution::json_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["required"],
            serde_json::json!(["ok", "chk", "fx", "fail"])
        );

        let variants = schema["$defs"]["WorldEffect"]["anyOf"]
            .as_array()
            .expect("WorldEffect anyOf array");
        let ops: Vec<&str> = variants
            .iter()
            .filter_map(|variant| variant["properties"]["op"]["enum"][0].as_str())
            .collect();
        assert_eq!(
            ops,
            [
                "narrate",
                "speak",
                "move",
                "modify",
                "spawn",
                "destroy",
                "history",
                "bubble",
                "award",
                "take",
                "place_item",
                "damage",
                "heal",
                "temp_hp",
                "condition",
                "apply_effect",
                "remove_effect",
                "spend_resource",
                "restore_resource",
                "advance_level",
            ]
        );
        for variant in variants {
            assert_eq!(variant["additionalProperties"], false);
        }
        assert_every_object_requires_all_properties(&schema);
    }

    /// Prevents exponential structured-output grammar state explosion (Gemini HTTP 400):
    /// every JSON Schema object must list all of its declared properties in `required`.
    fn assert_every_object_requires_all_properties(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                if map.get("type").and_then(serde_json::Value::as_str) == Some("object")
                    && let Some(props) =
                        map.get("properties").and_then(serde_json::Value::as_object)
                {
                    let required: std::collections::BTreeSet<&str> = map
                        .get("required")
                        .and_then(serde_json::Value::as_array)
                        .map_or_default(|arr| {
                            arr.iter().filter_map(serde_json::Value::as_str).collect()
                        });
                    let declared: std::collections::BTreeSet<&str> =
                        props.keys().map(String::as_str).collect();
                    assert_eq!(
                        required, declared,
                        "every object schema must require all declared properties to keep provider grammar states bounded"
                    );
                }
                for child in map.values() {
                    assert_every_object_requires_all_properties(child);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    assert_every_object_requires_all_properties(item);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn documented_property_effects_parse_and_match_wire_ops() {
        let samples = [
            r#"{"op": "damage", "id": 2, "amount": 8, "damage_type": "fire"}"#,
            r#"{"op": "heal", "id": 0, "amount": 6}"#,
            r#"{"op": "temp_hp", "id": 0, "amount": 5}"#,
            r#"{"op": "condition", "id": 2, "add": ["prone"], "exhaustion_delta": 1, "add_hazards": ["burning"]}"#,
            r#"{"op": "apply_effect", "id": 0, "effect": {"id": "bless", "name": "Bless", "source": "bless", "duration_rounds": 10, "modifiers": [{"kind": "roll_bonus", "dice": {"count": 1, "sides": 4}, "roll_scope": "attack_rolls"}]}, "concentrate": true}"#,
            r#"{"op": "remove_effect", "id": 0, "effect_id": "bless"}"#,
            r#"{"op": "spend_resource", "id": 0, "target": {"kind": "class_pool", "resource": "second_wind"}, "amount": 1}"#,
            r#"{"op": "restore_resource", "id": 0, "target": {"kind": "spell_slot", "spell_level": 1}, "amount": 1}"#,
            r#"{"op": "advance_level", "id": 0, "class": "fighter", "subclass": "champion"}"#,
        ];
        for raw in samples {
            let parsed = effect(raw);
            let value = serde_json::to_value(&parsed).expect("serializes");
            assert_eq!(value["op"], parsed.op());
            assert_eq!(parsed.referenced_ids().len(), 1);
        }
    }

    #[test]
    fn documented_item_examples_parse() {
        assert_eq!(
            effect(r#"{"op": "award", "id": 0, "item": "rope", "custom": null, "qty": 1}"#),
            WorldEffect::Award {
                id: 0,
                item: Some("rope".into()),
                custom: None,
                qty: 1,
            }
        );
        assert_eq!(
            effect(
                r#"{"op": "award", "id": 0, "item": null, "custom": {"name": "Sealed Letter", "weight_thousandths_of_pound": 10, "description": "A letter sealed with the baron's crest.", "tags": ["sealed"]}, "qty": 1}"#
            ),
            WorldEffect::Award {
                id: 0,
                item: None,
                custom: Some(CustomItem {
                    name: "Sealed Letter".into(),
                    weight_thousandths_of_pound: 10,
                    description: "A letter sealed with the baron's crest.".into(),
                    tags: ["sealed".to_owned()].into(),
                }),
                qty: 1,
            }
        );
        let take =
            effect(r#"{"op": "take", "id": 3, "item": "rope", "custom_name": null, "qty": 1}"#);
        assert_eq!(
            take,
            WorldEffect::Take {
                id: 3,
                item: Some("rope".into()),
                custom_name: None,
                qty: 1,
            }
        );
        assert_eq!(take.referenced_ids(), [3]);
        assert_eq!(
            serde_json::to_value(&take).expect("serializes")["op"],
            take.op()
        );
        let place = effect(
            r#"{"op": "place_item", "from": 0, "x": 14, "y": 20, "item": "longsword", "custom": null, "qty": 1}"#,
        );
        assert_eq!(
            place,
            WorldEffect::PlaceItem {
                from: Some(0),
                x: 14,
                y: 20,
                item: Some("longsword".into()),
                custom: None,
                qty: 1,
            }
        );
        assert_eq!(place.referenced_ids(), [0]);
        assert_eq!(place.target_square(), Some((14, 20)));
        assert_eq!(
            serde_json::to_value(&place).expect("serializes")["op"],
            place.op()
        );
    }

    #[test]
    fn custom_items_reject_combat_fields() {
        let armed = r#"{"op": "award", "id": 0, "item": null, "custom": {"name": "Blade", "weight_thousandths_of_pound": 10, "description": "A blade.", "tags": [], "damage": "1d8"}, "qty": 1}"#;
        assert!(serde_json::from_str::<WorldEffect>(armed).is_err());
    }
}
