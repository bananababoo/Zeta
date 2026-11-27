//! Structural validation of AI DM responses.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Section 4. Only structure is checked: the
//! schema, that every identifier exists in the Snapshot, and that every coordinate is inside
//! the map. Every message is written for the AI DM to read and correct on retry, so it names
//! the exact field and the allowed values.

use std::fmt;

use thiserror::Error;

use crate::catalog::{Catalog, CatalogId};
use crate::grid::MapBounds;
use crate::protocol::inventory::CustomItem;
use crate::protocol::properties::{MAX_EXHAUSTION_LEVEL, ResourceTarget, ResourceTargetKind};
use crate::resolution::{AiResolution, Branch, LocalId, WorldEffect};
use crate::settings::{
    CUSTOM_ITEM_DESCRIPTION_MAX_CHARACTERS, CUSTOM_ITEM_MAX_WEIGHT_THOUSANDTHS_OF_POUND,
    CUSTOM_ITEM_NAME_MAX_CHARACTERS, CUSTOM_ITEM_TAG_MAX_CHARACTERS, CUSTOM_ITEM_TAG_MAX_COUNT,
    ITEM_EFFECT_MAX_QUANTITY,
};
use crate::snapshot::Snapshot;

/// Where in a response an effect sits, written the way the AI DM sees it, such as `fx[2]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectLocation {
    /// The list holding the effect.
    pub branch: Branch,
    /// Position in that list, from 0.
    pub index: usize,
}

impl fmt::Display for EffectLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = match self.branch {
            Branch::Success => "fx",
            Branch::Failure => "fail",
        };
        write!(formatter, "{list}[{}]", self.index)
    }
}

/// 1 structural problem in an AI DM response.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    /// The text is not JSON matching the AI Resolution schema.
    #[error("the response does not match the schema: {message}")]
    Schema {
        /// The parser's description of the problem.
        message: String,
    },
    /// An effect names an identifier that is not in the Snapshot.
    #[error(
        "{location} ({op}): id {id} is not in the Snapshot; valid ids are 0 to {highest_valid_id}"
    )]
    UnknownId {
        /// The effect.
        location: EffectLocation,
        /// The effect's `op` tag.
        op: &'static str,
        /// The identifier that does not exist.
        id: LocalId,
        /// The highest identifier in the Snapshot.
        highest_valid_id: usize,
    },
    /// An effect places something outside the map.
    #[error(
        "{location} ({op}): square ({x}, {y}) is outside the map; x must be below {width} and y below {height}"
    )]
    OutOfBounds {
        /// The effect.
        location: EffectLocation,
        /// The effect's `op` tag.
        op: &'static str,
        /// Requested column.
        x: u32,
        /// Requested row.
        y: u32,
        /// Map width in squares.
        width: u32,
        /// Map height in squares.
        height: u32,
    },
    /// `fail` holds effects although `chk` is null.
    #[error("fail must be empty when chk is null, because nothing can fail without a check")]
    FailWithoutCheck,
    /// An `award` or `take` effect names an item the rules do not allow.
    #[error("{location} ({op}): {problem}")]
    Item {
        /// The effect.
        location: EffectLocation,
        /// The effect's `op` tag.
        op: &'static str,
        /// What is wrong with the item.
        problem: ItemProblem,
    },
    /// A 5e property effect breaks a structural rules invariant.
    #[error("{location} ({op}): {problem}")]
    Property {
        /// The effect.
        location: EffectLocation,
        /// The effect's `op` tag.
        op: &'static str,
        /// What is wrong with the property effect.
        problem: PropertyProblem,
    },
}

/// What is wrong with a 5e property `WorldEffect` (`docs/architecture/entity_properties.md`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PropertyProblem {
    /// A numeric amount was 0 when at least 1 is required.
    #[error("{field} must be at least 1")]
    ZeroAmount {
        /// Name of the field.
        field: &'static str,
    },
    /// `exhaustion_delta` exceeds the 2024 5e exhaustion range (`-6..=6`).
    #[error("exhaustion_delta {delta} must be between -6 and 6")]
    ExhaustionDeltaOutOfRange {
        /// Requested delta.
        delta: i8,
    },
    /// A required identifier or label was empty.
    #[error("{field} must not be empty")]
    EmptyText {
        /// Name of the field.
        field: &'static str,
    },
    /// A `ResourceTarget` omitted the selector required by its `kind`.
    #[error("target must set {expected} for its resource kind")]
    InvalidResourceTarget {
        /// Name of the required target field.
        expected: &'static str,
    },
    /// `target.spell_level` was outside `1..=9`.
    #[error("target.spell_level {level} must be from 1 to 9")]
    InvalidSpellLevel {
        /// Requested spell level.
        level: u8,
    },
    /// `advance_level` paired a subclass with a different class.
    #[error("subclass \"{subclass}\" belongs to \"{expected_class}\", not \"{class}\"")]
    SubclassMismatch {
        /// Subclass provided.
        subclass: &'static str,
        /// Class provided.
        class: &'static str,
        /// Owning class of the subclass.
        expected_class: &'static str,
    },
}

/// What is wrong with the item in an `award` or `take` effect
/// (`docs/design/game_design.md` Section 5).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ItemProblem {
    /// Neither or both item kinds were set.
    #[error("set exactly 1 of {catalog_field} or {custom_field}")]
    ItemKind {
        /// The catalog id field.
        catalog_field: &'static str,
        /// The custom item field.
        custom_field: &'static str,
    },
    /// `qty` is 0 or too large.
    #[error("qty {qty} must be from 1 to {max}")]
    Quantity {
        /// Requested quantity.
        qty: u32,
        /// Largest allowed quantity.
        max: u32,
    },
    /// The catalog id does not exist.
    #[error(
        "item \"{id}\" is not in the item catalog; use an exact catalog id, or a custom item only for a unique story object"
    )]
    UnknownCatalogId {
        /// The id given.
        id: String,
    },
    /// The catalog id names an item group, which is not a real item.
    #[error("item \"{id}\" is a group, not an item; use 1 of: {members}")]
    ItemGroup {
        /// The group id.
        id: String,
        /// Comma-separated member ids.
        members: String,
    },
    /// The custom name is empty or too long.
    #[error("custom.name must have 1 to {max} characters")]
    NameLength {
        /// Longest allowed name.
        max: usize,
    },
    /// The custom item copies a catalog item, which must be awarded by id instead.
    #[error("custom item \"{name}\" is a catalog item; set item to \"{id}\" and custom to null")]
    CatalogDuplicate {
        /// The custom name.
        name: String,
        /// The matching catalog id.
        id: String,
    },
    /// The description is empty, too long, or more than 1 sentence.
    #[error("custom.description must be 1 sentence of at most {max} characters")]
    Description {
        /// Longest allowed description.
        max: usize,
    },
    /// Too many tags.
    #[error("custom.tags may hold at most {max} tags")]
    TagCount {
        /// Most tags allowed.
        max: usize,
    },
    /// A tag is empty or too long.
    #[error("custom.tags must each have 1 to {max} characters")]
    TagLength {
        /// Longest allowed tag.
        max: usize,
    },
    /// The item is heavier than a narrative object may be.
    #[error("custom.weight_thousandths_of_pound must be at most {max}")]
    Weight {
        /// Heaviest allowed weight.
        max: u32,
    },
}

/// Every problem found in 1 response, in the order they appear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationErrors(pub Vec<ValidationError>);

impl fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (number, error) in self.0.iter().enumerate() {
            if number > 0 {
                writeln!(formatter)?;
            }
            write!(formatter, "- {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

/// Parses AI DM output and checks its structure against the request's Snapshot, map, and
/// item catalog.
///
/// # Errors
///
/// Returns every problem found. A schema problem stops parsing, so it is reported alone.
pub fn parse_and_validate(
    text: &str,
    snapshot: &Snapshot,
    bounds: MapBounds,
    catalog: &Catalog,
) -> Result<AiResolution, ValidationErrors> {
    let resolution: AiResolution = serde_json::from_str(text).map_err(|error| {
        ValidationErrors(vec![ValidationError::Schema {
            message: error.to_string(),
        }])
    })?;
    validate(&resolution, snapshot, bounds, catalog)?;
    Ok(resolution)
}

/// Checks the structure of an already parsed resolution.
///
/// # Errors
///
/// Returns every problem found, in the order they appear in the response.
pub fn validate(
    resolution: &AiResolution,
    snapshot: &Snapshot,
    bounds: MapBounds,
    catalog: &Catalog,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    if resolution.chk.is_none() && !resolution.fail.is_empty() {
        errors.push(ValidationError::FailWithoutCheck);
    }
    for branch in [Branch::Success, Branch::Failure] {
        for (index, effect) in resolution.effects(branch).iter().enumerate() {
            let location = EffectLocation { branch, index };
            check_effect(effect, location, snapshot, bounds, &mut errors);
            errors.extend(item_problems(effect, catalog).into_iter().map(|problem| {
                ValidationError::Item {
                    location,
                    op: effect.op(),
                    problem,
                }
            }));
            errors.extend(property_problems(effect).into_iter().map(|problem| {
                ValidationError::Property {
                    location,
                    op: effect.op(),
                    problem,
                }
            }));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors(errors))
    }
}

/// Property rule problems in 1 5e property effect; empty for every other effect.
fn property_problems(effect: &WorldEffect) -> Vec<PropertyProblem> {
    let mut problems = Vec::new();
    let max_exhaustion = i8::try_from(MAX_EXHAUSTION_LEVEL).unwrap_or(6);
    match effect {
        WorldEffect::Damage { amount, .. }
        | WorldEffect::Heal { amount, .. }
        | WorldEffect::TempHp { amount, .. } => {
            if *amount == 0 {
                problems.push(PropertyProblem::ZeroAmount { field: "amount" });
            }
        }
        WorldEffect::Condition {
            exhaustion_delta, ..
        } => {
            if !(-max_exhaustion..=max_exhaustion).contains(exhaustion_delta) {
                problems.push(PropertyProblem::ExhaustionDeltaOutOfRange {
                    delta: *exhaustion_delta,
                });
            }
        }
        WorldEffect::ApplyEffect { effect, .. } => {
            if effect.id.trim().is_empty() {
                problems.push(PropertyProblem::EmptyText { field: "effect.id" });
            }
            if effect.name.trim().is_empty() {
                problems.push(PropertyProblem::EmptyText {
                    field: "effect.name",
                });
            }
        }
        WorldEffect::RemoveEffect { effect_id, .. } => {
            if effect_id.trim().is_empty() {
                problems.push(PropertyProblem::EmptyText { field: "effect_id" });
            }
        }
        WorldEffect::SpendResource { target, amount, .. }
        | WorldEffect::RestoreResource { target, amount, .. } => {
            if *amount == 0 {
                problems.push(PropertyProblem::ZeroAmount { field: "amount" });
            }
            problems.extend(resource_target_problem(target));
        }
        WorldEffect::AdvanceLevel {
            class, subclass, ..
        } => {
            if let Some(sub) = subclass
                && sub.parent_class() != *class
            {
                problems.push(PropertyProblem::SubclassMismatch {
                    subclass: sub.as_str(),
                    class: class.as_str(),
                    expected_class: sub.parent_class().as_str(),
                });
            }
        }
        _ => {}
    }
    problems
}

fn resource_target_problem(target: &ResourceTarget) -> Option<PropertyProblem> {
    match target.kind {
        ResourceTargetKind::SpellSlot => match target.spell_level {
            None => Some(PropertyProblem::InvalidResourceTarget {
                expected: "spell_level",
            }),
            Some(level) if !(1..=9).contains(&level) => {
                Some(PropertyProblem::InvalidSpellLevel { level })
            }
            Some(_) => None,
        },
        ResourceTargetKind::PactSlot => None,
        ResourceTargetKind::ClassPool => {
            target
                .resource
                .is_none()
                .then_some(PropertyProblem::InvalidResourceTarget {
                    expected: "resource",
                })
        }
        ResourceTargetKind::FeatureUse => {
            target
                .feature_id
                .is_none()
                .then_some(PropertyProblem::InvalidResourceTarget {
                    expected: "feature_id",
                })
        }
        ResourceTargetKind::ItemCharge => {
            target
                .stack_id
                .is_none()
                .then_some(PropertyProblem::InvalidResourceTarget {
                    expected: "stack_id",
                })
        }
    }
}

/// Item rule problems in 1 `award` or `take` effect; empty for every other effect.
fn item_problems(effect: &WorldEffect, catalog: &Catalog) -> Vec<ItemProblem> {
    let mut problems = Vec::new();
    let (item, has_custom, qty) = match effect {
        WorldEffect::Award {
            item, custom, qty, ..
        } => {
            if item.is_some() == custom.is_some() {
                problems.push(ItemProblem::ItemKind {
                    catalog_field: "item",
                    custom_field: "custom",
                });
            }
            if let Some(custom) = custom {
                problems.extend(custom_item_problems(custom, catalog));
            }
            (item, custom.is_some(), *qty)
        }
        WorldEffect::Take {
            item,
            custom_name,
            qty,
            ..
        } => {
            if item.is_some() == custom_name.is_some() {
                problems.push(ItemProblem::ItemKind {
                    catalog_field: "item",
                    custom_field: "custom_name",
                });
            }
            (item, custom_name.is_some(), *qty)
        }
        _ => return problems,
    };
    if qty == 0 || qty > ITEM_EFFECT_MAX_QUANTITY {
        problems.push(ItemProblem::Quantity {
            qty,
            max: ITEM_EFFECT_MAX_QUANTITY,
        });
    }
    if let Some(id) = item
        && !has_custom
    {
        problems.extend(catalog_id_problem(id, catalog));
    }
    problems
}

/// The problem with an AI-given catalog id, if any: it must name a real, non-group entry.
fn catalog_id_problem(id: &str, catalog: &Catalog) -> Option<ItemProblem> {
    let entry = CatalogId::new(id)
        .ok()
        .and_then(|catalog_id| catalog.get(&catalog_id));
    match entry {
        None => Some(ItemProblem::UnknownCatalogId { id: id.to_owned() }),
        Some(entry) if entry.is_group() => Some(ItemProblem::ItemGroup {
            id: id.to_owned(),
            members: entry
                .group_members
                .iter()
                .map(CatalogId::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        }),
        Some(_) => None,
    }
}

/// Limits on custom narrative items: a short name that is not a catalog item, 1 short
/// sentence, and a few tags. Combat fields cannot exist; the schema rejects them.
fn custom_item_problems(custom: &CustomItem, catalog: &Catalog) -> Vec<ItemProblem> {
    let mut problems = Vec::new();
    let name = custom.name.trim();
    if name.is_empty() || name.chars().count() > CUSTOM_ITEM_NAME_MAX_CHARACTERS {
        problems.push(ItemProblem::NameLength {
            max: CUSTOM_ITEM_NAME_MAX_CHARACTERS,
        });
    }
    let lowered = name.to_lowercase();
    if let Some(entry) = catalog
        .items()
        .find(|entry| entry.name.to_lowercase() == lowered || entry.id.as_str() == lowered)
    {
        problems.push(ItemProblem::CatalogDuplicate {
            name: name.to_owned(),
            id: entry.id.to_string(),
        });
    }
    if !is_one_sentence(&custom.description)
        || custom.description.chars().count() > CUSTOM_ITEM_DESCRIPTION_MAX_CHARACTERS
    {
        problems.push(ItemProblem::Description {
            max: CUSTOM_ITEM_DESCRIPTION_MAX_CHARACTERS,
        });
    }
    if custom.tags.len() > CUSTOM_ITEM_TAG_MAX_COUNT {
        problems.push(ItemProblem::TagCount {
            max: CUSTOM_ITEM_TAG_MAX_COUNT,
        });
    }
    if custom
        .tags
        .iter()
        .any(|tag| tag.trim().is_empty() || tag.chars().count() > CUSTOM_ITEM_TAG_MAX_CHARACTERS)
    {
        problems.push(ItemProblem::TagLength {
            max: CUSTOM_ITEM_TAG_MAX_CHARACTERS,
        });
    }
    if custom.weight_thousandths_of_pound > CUSTOM_ITEM_MAX_WEIGHT_THOUSANDTHS_OF_POUND {
        problems.push(ItemProblem::Weight {
            max: CUSTOM_ITEM_MAX_WEIGHT_THOUSANDTHS_OF_POUND,
        });
    }
    problems
}

/// True for non-empty text with no sentence end (`.`, `!`, `?`) followed by more text.
fn is_one_sentence(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && !text.char_indices().any(|(index, character)| {
            matches!(character, '.' | '!' | '?')
                && text[index + character.len_utf8()..].starts_with(char::is_whitespace)
        })
}

fn check_effect(
    effect: &WorldEffect,
    location: EffectLocation,
    snapshot: &Snapshot,
    bounds: MapBounds,
    errors: &mut Vec<ValidationError>,
) {
    for &id in effect.referenced_ids() {
        if !snapshot.contains(id) {
            errors.push(ValidationError::UnknownId {
                location,
                op: effect.op(),
                id,
                highest_valid_id: snapshot.len().saturating_sub(1),
            });
        }
    }
    let local_bounds = snapshot.local_bounds(bounds);
    if let Some((x, y)) = effect.target_square()
        && !local_bounds.contains(x, y)
    {
        errors.push(ValidationError::OutOfBounds {
            location,
            op: effect.op(),
            x,
            y,
            width: local_bounds.width,
            height: local_bounds.height,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::tests::entity;
    use crate::snapshot::{PerceptionSettings, Snapshot};

    const BOUNDS: MapBounds = MapBounds {
        width: 20,
        height: 10,
    };

    /// A Snapshot with local identifiers 0, 1, and 2.
    fn snapshot() -> Snapshot {
        Snapshot::build(
            entity(10, 0, 0),
            [entity(11, 1, 0), entity(12, 2, 0)],
            |_| false,
            PerceptionSettings {
                hearing_range_squares: 5,
                max_count: 10,
            },
        )
    }

    fn errors(text: &str) -> Vec<ValidationError> {
        parse_and_validate(text, &snapshot(), BOUNDS, Catalog::srd())
            .expect_err("response should be rejected")
            .0
    }

    #[test]
    fn valid_response_is_accepted() {
        let text = r#"{
            "ok": true,
            "chk": {"dc": 12, "reason": "the guard is alert"},
            "fx": [{"op": "move", "id": 0, "x": 19, "y": 9, "style": "sneaks"}],
            "fail": [{"op": "speak", "id": 2, "text": "Halt!", "vol": "shout"}]
        }"#;
        assert!(parse_and_validate(text, &snapshot(), BOUNDS, Catalog::srd()).is_ok());
    }

    /// Wraps effects into an automatic response.
    fn with_effects(effects: &str) -> String {
        format!(r#"{{"ok": true, "chk": null, "fx": [{effects}], "fail": []}}"#)
    }

    fn item_messages(effects: &str) -> Vec<String> {
        errors(&with_effects(effects))
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn documented_item_effects_are_accepted() {
        let text = with_effects(
            r#"{"op": "award", "id": 0, "item": "rope", "custom": null, "qty": 1},
            {"op": "award", "id": 0, "item": null, "custom": {"name": "Sealed Letter", "weight_thousandths_of_pound": 10, "description": "A letter sealed with the baron's crest.", "tags": ["sealed"]}, "qty": 1},
            {"op": "take", "id": 2, "item": "rope", "custom_name": null, "qty": 1}"#,
        );
        assert!(parse_and_validate(&text, &snapshot(), BOUNDS, Catalog::srd()).is_ok());
    }

    #[test]
    fn awarded_catalog_ids_must_exist_and_not_be_groups() {
        let messages = item_messages(
            r#"{"op": "award", "id": 0, "item": "lightsaber", "custom": null, "qty": 1},
            {"op": "award", "id": 0, "item": "holy_symbol", "custom": null, "qty": 1}"#,
        );
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(
            messages[0]
                .starts_with("fx[0] (award): item \"lightsaber\" is not in the item catalog")
        );
        assert!(messages[1].starts_with("fx[1] (award): item \"holy_symbol\" is a group"));
    }

    #[test]
    fn item_effects_need_1_item_kind_and_a_quantity() {
        let messages = item_messages(
            r#"{"op": "award", "id": 0, "item": null, "custom": null, "qty": 0},
            {"op": "take", "id": 0, "item": "rope", "custom_name": "Rope", "qty": 1}"#,
        );
        assert_eq!(
            messages,
            [
                "fx[0] (award): set exactly 1 of item or custom",
                "fx[0] (award): qty 0 must be from 1 to 100",
                "fx[1] (take): set exactly 1 of item or custom_name",
            ]
        );
    }

    #[test]
    fn custom_items_stay_narrative() {
        let messages = item_messages(
            r#"{"op": "award", "id": 0, "item": null, "custom": {"name": "Rope", "weight_thousandths_of_pound": 10, "description": "A rope. It is long.", "tags": ["a", "b", "c", "d", "e", "f", "g", "h", "i"]}, "qty": 1}"#,
        );
        assert_eq!(
            messages,
            [
                "fx[0] (award): custom item \"Rope\" is a catalog item; set item to \"rope\" and custom to null",
                "fx[0] (award): custom.description must be 1 sentence of at most 200 characters",
                "fx[0] (award): custom.tags may hold at most 8 tags",
            ]
        );
    }

    #[test]
    fn custom_item_tags_and_weight_are_capped() {
        let messages = item_messages(
            r#"{"op": "award", "id": 0, "item": null, "custom": {"name": "Iron Idol", "weight_thousandths_of_pound": 50001, "description": "A squat idol of black iron.", "tags": ["", "an_extremely_long_tag_that_goes_past_the_limit"]}, "qty": 1}"#,
        );
        assert_eq!(
            messages,
            [
                "fx[0] (award): custom.tags must each have 1 to 32 characters",
                "fx[0] (award): custom.weight_thousandths_of_pound must be at most 50000",
            ]
        );
    }

    #[test]
    fn one_sentence_allows_inner_punctuation() {
        assert!(is_one_sentence("A key, worn smooth by time (and hands)."));
        assert!(!is_one_sentence("A key. A lock."));
        assert!(!is_one_sentence("  "));
    }

    #[test]
    fn malformed_json_is_a_schema_error() {
        assert!(matches!(
            errors("{\"ok\": tru").as_slice(),
            [ValidationError::Schema { .. }]
        ));
    }

    #[test]
    fn every_unknown_id_and_bad_square_is_reported_with_its_location() {
        let text = r#"{
            "ok": true,
            "chk": {"dc": 10, "reason": "test"},
            "fx": [
                {"op": "narrate", "text": "fine"},
                {"op": "history", "ids": [1, 7], "summary": "s", "preserved": false}
            ],
            "fail": [{"op": "spawn", "name": "Smoke", "x": 20, "y": 3, "passable": true, "tags": [], "visual": "smoke", "settle": null}]
        }"#;
        let messages: Vec<String> = errors(text).iter().map(ToString::to_string).collect();
        assert_eq!(
            messages,
            [
                "fx[1] (history): id 7 is not in the Snapshot; valid ids are 0 to 2",
                "fail[0] (spawn): square (20, 3) is outside the map; x must be below 20 and y below 10",
            ]
        );
    }

    #[test]
    fn fail_effects_without_a_check_are_rejected() {
        let text =
            r#"{"ok": true, "chk": null, "fx": [], "fail": [{"op": "narrate", "text": "x"}]}"#;
        assert_eq!(errors(text), [ValidationError::FailWithoutCheck]);
    }

    #[test]
    fn errors_join_into_one_retry_message() {
        let joined = ValidationErrors(vec![
            ValidationError::FailWithoutCheck,
            ValidationError::Schema {
                message: "m".into(),
            },
        ])
        .to_string();
        assert_eq!(joined.lines().count(), 2);
        assert!(joined.lines().all(|line| line.starts_with("- ")));
    }

    #[test]
    fn valid_property_effects_are_accepted() {
        let text = with_effects(
            r#"{"op": "damage", "id": 1, "amount": 7, "damage_type": "slashing"},
            {"op": "heal", "id": 0, "amount": 5},
            {"op": "temp_hp", "id": 0, "amount": 4},
            {"op": "condition", "id": 1, "add": ["prone"], "exhaustion_delta": 1},
            {"op": "apply_effect", "id": 0, "effect": {"id": "bless", "name": "Bless"}, "concentrate": true},
            {"op": "remove_effect", "id": 0, "effect_id": "bless"},
            {"op": "spend_resource", "id": 0, "target": {"kind": "class_pool", "resource": "second_wind"}, "amount": 1},
            {"op": "restore_resource", "id": 0, "target": {"kind": "spell_slot", "spell_level": 1}, "amount": 1},
            {"op": "advance_level", "id": 0, "class": "fighter", "subclass": "champion"}"#,
        );
        assert!(parse_and_validate(&text, &snapshot(), BOUNDS, Catalog::srd()).is_ok());
    }

    #[test]
    fn invalid_property_effects_report_exact_problems() {
        let messages = item_messages(
            r#"{"op": "damage", "id": 1, "amount": 0, "damage_type": "fire"},
            {"op": "condition", "id": 1, "exhaustion_delta": 9},
            {"op": "spend_resource", "id": 0, "target": {"kind": "spell_slot", "spell_level": 10}, "amount": 1},
            {"op": "advance_level", "id": 0, "class": "wizard", "subclass": "champion"}"#,
        );
        assert_eq!(
            messages,
            [
                "fx[0] (damage): amount must be at least 1",
                "fx[1] (condition): exhaustion_delta 9 must be between -6 and 6",
                "fx[2] (spend_resource): target.spell_level 10 must be from 1 to 9",
                "fx[3] (advance_level): subclass \"champion\" belongs to \"fighter\", not \"wizard\"",
            ]
        );
    }
}
