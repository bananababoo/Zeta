//! Composable 2024 5e (System Reference Document 5.2.1) ECS property components and closed
//! rule enums.
//!
//! Rules: `docs/architecture/entity_properties.md`. Every replicated type here uses plain
//! structs and unit-only enums so `bevy_replicon` (`postcard`) encodes and decodes them cleanly.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::catalog::{Ability, ArmorCategory, CatalogId, DamageType, Dice, Skill, WeaponCategory};
use crate::protocol::inventory::StackId;

/// Longest identifier a [`RulesetId`] may have.
const MAX_RULESET_ID_LENGTH: usize = 64;

/// Maximum total character level across all classes in 5e.
pub const MAX_CHARACTER_LEVEL: u8 = 20;

/// Maximum exhaustion level before death in 2024 5e (SRD 5.2.1).
pub const MAX_EXHAUSTION_LEVEL: u8 = 6;

/// Maximum attuned magic item stacks a creature may hold simultaneously.
pub const MAX_ATTUNED_ITEMS: usize = 3;

/// Stable lowercase identifier for a feature, trait, spell, or rule in `srd_ruleset.json`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RulesetId(String);

/// Text that cannot be a [`RulesetId`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error(
    "\"{0}\" is not a ruleset id; ids are 1 to 64 characters of lowercase a-z, digits 0-9, and _"
)]
pub struct InvalidRulesetId(pub String);

impl RulesetId {
    /// Wraps and validates a ruleset identifier.
    ///
    /// # Errors
    /// Returns [`InvalidRulesetId`] unless `id` is 1 to 64 lowercase `a-z`, `0-9`, or `_` chars.
    pub fn new(id: impl Into<String>) -> Result<Self, InvalidRulesetId> {
        let id = id.into();
        let valid = id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if valid && !id.is_empty() && id.len() <= MAX_RULESET_ID_LENGTH {
            Ok(Self(id))
        } else {
            Err(InvalidRulesetId(id))
        }
    }

    /// Underlying identifier string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RulesetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for RulesetId {
    type Error = InvalidRulesetId;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        Self::new(id)
    }
}

impl From<RulesetId> for String {
    fn from(id: RulesetId) -> Self {
        id.0
    }
}

// ---------------------------------------------------------------------------
// Group 1: Shared Physical & Combat Vitals (Creatures and Objects)
// ---------------------------------------------------------------------------

/// Hit points for any creature, construct, or breakable object (bridge, door, barrel).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Health {
    /// Current hit points (`0..=max`).
    pub current: u16,
    /// Maximum hit points.
    pub max: u16,
    /// Non-stacking temporary hit points absorbed before `current`.
    pub temporary: u16,
}

/// Outcome of applying damage to a [`Health`] component using 2024 5e rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DamageOutcome {
    /// Effective damage after damage threshold, immunity, resistance, and vulnerability.
    pub effective_damage: u16,
    /// Damage absorbed by temporary hit points.
    pub absorbed_by_temp: u16,
    /// Damage subtracted from current hit points.
    pub hp_lost: u16,
    /// Whether `current` reached `0` as a result of this damage.
    pub reached_zero: bool,
}

impl Health {
    /// Creates a full-health pool with no temporary hit points.
    pub const fn full(max: u16) -> Self {
        Self {
            current: max,
            max,
            temporary: 0,
        }
    }

    /// 2024 5e Bloodied rule: `true` while current HP is half of maximum or fewer.
    pub const fn is_bloodied(self) -> bool {
        self.max > 0 && self.current <= self.max / 2
    }

    /// Whether the entity is at 0 hit points.
    pub const fn is_zero(self) -> bool {
        self.current == 0
    }

    /// Applies damage with optional `damage_threshold`, `DamageAffinities`, and `ActiveEffects`.
    pub fn apply_damage(
        &mut self,
        raw_amount: u16,
        damage_type: DamageType,
        damage_threshold: u16,
        affinities: Option<&DamageAffinities>,
        effects: Option<&ActiveEffects>,
    ) -> DamageOutcome {
        let immune = affinities.is_some_and(|a| a.immunities.contains(&damage_type))
            || effects.is_some_and(|e| e.has_damage_immunity(damage_type));
        if immune || raw_amount < damage_threshold {
            return DamageOutcome {
                effective_damage: 0,
                absorbed_by_temp: 0,
                hp_lost: 0,
                reached_zero: false,
            };
        }

        let resistant = affinities.is_some_and(|a| a.resistances.contains(&damage_type))
            || effects.is_some_and(|e| e.has_damage_resistance(damage_type));
        let vulnerable = affinities.is_some_and(|a| a.vulnerabilities.contains(&damage_type))
            || effects.is_some_and(|e| e.has_damage_vulnerability(damage_type));

        let mut effective = raw_amount;
        if resistant && !vulnerable {
            effective /= 2;
        } else if vulnerable && !resistant {
            effective = effective.saturating_mul(2);
        }

        let absorbed_by_temp = self.temporary.min(effective);
        self.temporary -= absorbed_by_temp;
        let remaining = effective - absorbed_by_temp;
        let hp_lost = self.current.min(remaining);
        self.current -= hp_lost;

        DamageOutcome {
            effective_damage: effective,
            absorbed_by_temp,
            hp_lost,
            reached_zero: self.current == 0 && effective > 0,
        }
    }

    /// Restores up to `amount` hit points, clamped to `max`. Returns actual HP restored.
    pub fn apply_heal(&mut self, amount: u16) -> u16 {
        let before = self.current;
        self.current = self.current.saturating_add(amount).min(self.max);
        self.current - before
    }

    /// Applies temporary hit points under the 5e non-stacking rule (`max(existing, amount)`).
    pub fn apply_temp_hp(&mut self, amount: u16) {
        self.temporary = self.temporary.max(amount);
    }
}

/// Armor Class and optional structural damage threshold for creatures and breakable objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct ArmorClass {
    /// Base Armor Class before temporary [`ActiveEffects`] bonuses.
    pub base: u8,
    /// Minimum single-hit damage required to harm a fortified structure (`0` = no threshold).
    pub damage_threshold: u16,
}

impl ArmorClass {
    /// Standard Armor Class with no structural damage threshold.
    pub const fn new(base: u8) -> Self {
        Self {
            base,
            damage_threshold: 0,
        }
    }

    /// Effective Armor Class including flat AC bonuses from `effects`.
    pub fn effective(self, effects: Option<&ActiveEffects>) -> u8 {
        let bonus = effects.map_or(0, ActiveEffects::total_ac_bonus);
        let total = i16::from(self.base).saturating_add(bonus).clamp(0, 255);
        u8::try_from(total).unwrap_or(u8::MAX)
    }
}

/// The 6 creature and object size categories in SRD 5.2.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub enum Size {
    /// 2.5 by 2.5 feet (bottle, sprite).
    Tiny,
    /// 5 by 5 feet (goblin, halfling, chest).
    Small,
    /// 5 by 5 feet (human, barrel).
    Medium,
    /// 10 by 10 feet (ogre, horse, cart).
    Large,
    /// 15 by 15 feet (giant, adult dragon).
    Huge,
    /// 20 by 20 feet or larger (ancient dragon, ship).
    Gargantuan,
}

impl Size {
    /// All 6 sizes from smallest to largest.
    pub const ALL: [Self; 6] = [
        Self::Tiny,
        Self::Small,
        Self::Medium,
        Self::Large,
        Self::Huge,
        Self::Gargantuan,
    ];

    /// Canonical lowercase ruleset identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tiny => "tiny",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::Huge => "huge",
            Self::Gargantuan => "gargantuan",
        }
    }
}

/// Damage resistances, immunities, and vulnerabilities for a creature or object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct DamageAffinities {
    /// Damage types halved (rounded down).
    pub resistances: BTreeSet<DamageType>,
    /// Damage types reduced to 0.
    pub immunities: BTreeSet<DamageType>,
    /// Damage types doubled.
    pub vulnerabilities: BTreeSet<DamageType>,
}

impl DamageAffinities {
    /// Standard SRD 5.2.1 object affinities: immunity to Poison and Psychic damage.
    pub fn object_default() -> Self {
        Self {
            resistances: BTreeSet::new(),
            immunities: BTreeSet::from([DamageType::Poison, DamageType::Psychic]),
            vulnerabilities: BTreeSet::new(),
        }
    }
}

/// Conditions that cannot affect this entity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct ConditionImmunities {
    /// Conditions ignored when applied to this entity.
    pub immune: BTreeSet<ConditionId>,
}

// ---------------------------------------------------------------------------
// Group 2: Core Creature Anatomy & Senses
// ---------------------------------------------------------------------------

/// The 14 creature types in SRD 5.2.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub enum CreatureType {
    /// Aberration.
    Aberration,
    /// Beast.
    Beast,
    /// Celestial.
    Celestial,
    /// Construct.
    Construct,
    /// Dragon.
    Dragon,
    /// Elemental.
    Elemental,
    /// Fey.
    Fey,
    /// Fiend.
    Fiend,
    /// Giant.
    Giant,
    /// Humanoid.
    Humanoid,
    /// Monstrosity.
    Monstrosity,
    /// Ooze.
    Ooze,
    /// Plant.
    Plant,
    /// Undead.
    Undead,
}

impl CreatureType {
    /// All 14 SRD 5.2.1 creature types in alphabetical order.
    pub const ALL: [Self; 14] = [
        Self::Aberration,
        Self::Beast,
        Self::Celestial,
        Self::Construct,
        Self::Dragon,
        Self::Elemental,
        Self::Fey,
        Self::Fiend,
        Self::Giant,
        Self::Humanoid,
        Self::Monstrosity,
        Self::Ooze,
        Self::Plant,
        Self::Undead,
    ];

    /// Canonical lowercase ruleset identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aberration => "aberration",
            Self::Beast => "beast",
            Self::Celestial => "celestial",
            Self::Construct => "construct",
            Self::Dragon => "dragon",
            Self::Elemental => "elemental",
            Self::Fey => "fey",
            Self::Fiend => "fiend",
            Self::Giant => "giant",
            Self::Humanoid => "humanoid",
            Self::Monstrosity => "monstrosity",
            Self::Ooze => "ooze",
            Self::Plant => "plant",
            Self::Undead => "undead",
        }
    }
}

/// The 6 core ability scores (`1..=30`) of a creature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct AbilityScores {
    /// Strength score.
    pub strength: u8,
    /// Dexterity score.
    pub dexterity: u8,
    /// Constitution score.
    pub constitution: u8,
    /// Intelligence score.
    pub intelligence: u8,
    /// Wisdom score.
    pub wisdom: u8,
    /// Charisma score.
    pub charisma: u8,
}

impl Default for AbilityScores {
    fn default() -> Self {
        Self {
            strength: 10,
            dexterity: 10,
            constitution: 10,
            intelligence: 10,
            wisdom: 10,
            charisma: 10,
        }
    }
}

impl AbilityScores {
    /// Base score for `ability`.
    pub const fn score(self, ability: Ability) -> u8 {
        match ability {
            Ability::Strength => self.strength,
            Ability::Dexterity => self.dexterity,
            Ability::Constitution => self.constitution,
            Ability::Intelligence => self.intelligence,
            Ability::Wisdom => self.wisdom,
            Ability::Charisma => self.charisma,
        }
    }

    /// Standard 5e modifier `floor((score - 10) / 2)` for any ability score.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "u8 score in 0..=255 gives modifier in -5..=122, which always fits in i8"
    )]
    pub const fn score_modifier(score: u8) -> i8 {
        let diff = (score as i16) - 10;
        let floored = diff.div_euclid(2);
        floored as i8
    }

    /// Effective score for `ability` after applying [`ActiveEffects`] overrides and bonuses
    /// (such as a Belt of Giant Strength minimum override), clamped to `1..=30`.
    pub fn effective_score(self, ability: Ability, effects: Option<&ActiveEffects>) -> u8 {
        let mut score = i16::from(self.score(ability));
        if let Some(effects) = effects {
            for effect in &effects.effects {
                for modifier in &effect.modifiers {
                    if modifier.ability == Some(ability) {
                        match modifier.kind {
                            StatModifierKind::AbilityScoreBonus => {
                                score = score.saturating_add(modifier.value);
                            }
                            StatModifierKind::AbilityScoreMin => {
                                score = score.max(modifier.value);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        u8::try_from(score.clamp(1, 30)).unwrap_or(10)
    }

    /// Effective ability modifier after [`ActiveEffects`] overrides and bonuses.
    pub fn modifier(self, ability: Ability, effects: Option<&ActiveEffects>) -> i8 {
        Self::score_modifier(self.effective_score(ability, effects))
    }
}

/// Movement speeds in feet per round across all 5 movement modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Speed {
    /// Walking speed in feet.
    pub walk: u16,
    /// Burrowing speed in feet (`0` = none).
    pub burrow: u16,
    /// Climbing speed in feet (`0` = none).
    pub climb: u16,
    /// Flying speed in feet (`0` = none).
    pub fly: u16,
    /// Swimming speed in feet (`0` = none).
    pub swim: u16,
    /// Whether a flying creature hovers instead of falling when knocked Prone.
    pub hover: bool,
}

impl Default for Speed {
    fn default() -> Self {
        Self::walk_only(30)
    }
}

impl Speed {
    /// Standard walking-only speed in feet.
    pub const fn walk_only(walk: u16) -> Self {
        Self {
            walk,
            burrow: 0,
            climb: 0,
            fly: 0,
            swim: 0,
            hover: false,
        }
    }

    /// Effective walking speed in feet after [`Conditions`] exhaustion/zeroing and [`ActiveEffects`].
    pub fn effective_walk(
        self,
        conditions: Option<&Conditions>,
        effects: Option<&ActiveEffects>,
    ) -> u16 {
        if conditions.is_some_and(Conditions::zeros_speed) {
            return 0;
        }
        let exhaustion_penalty = i32::from(conditions.map_or(0, Conditions::speed_penalty_ft));
        let bonus = effects.map_or(0, ActiveEffects::total_speed_bonus_ft);
        let total = (i32::from(self.walk) + bonus - exhaustion_penalty).max(0);
        u16::try_from(total).unwrap_or(u16::MAX)
    }
}

/// Special perception ranges in feet (`0` = none).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Senses {
    /// Blindsight range in feet.
    pub blindsight: u16,
    /// Darkvision range in feet.
    pub darkvision: u16,
    /// Tremorsense range in feet.
    pub tremorsense: u16,
    /// Truesight range in feet.
    pub truesight: u16,
}

/// Degree of training in a skill.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProficiencyLevel {
    /// Half proficiency bonus rounded down (such as Jack of All Trades).
    Half,
    /// Full proficiency bonus.
    Proficient,
    /// Twice proficiency bonus (Expertise).
    Expertise,
}

/// Closed set of SRD 5.2.1 standard and rare languages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageId {
    /// Common.
    Common,
    /// Common Sign Language.
    CommonSignLanguage,
    /// Draconic.
    Draconic,
    /// Dwarvish.
    Dwarvish,
    /// Elvish.
    Elvish,
    /// Giant.
    Giant,
    /// Gnomish.
    Gnomish,
    /// Goblin.
    Goblin,
    /// Halfling.
    Halfling,
    /// Orc.
    Orc,
    /// Abyssal (rare).
    Abyssal,
    /// Celestial (rare).
    Celestial,
    /// Deep Speech (rare).
    DeepSpeech,
    /// Druidic (rare).
    Druidic,
    /// Infernal (rare).
    Infernal,
    /// Primordial (rare).
    Primordial,
    /// Sylvan (rare).
    Sylvan,
    /// Thieves' Cant (rare).
    ThievesCant,
    /// Undercommon (rare).
    Undercommon,
}

impl LanguageId {
    /// All 19 SRD 5.2.1 languages.
    pub const ALL: [Self; 19] = [
        Self::Common,
        Self::CommonSignLanguage,
        Self::Draconic,
        Self::Dwarvish,
        Self::Elvish,
        Self::Giant,
        Self::Gnomish,
        Self::Goblin,
        Self::Halfling,
        Self::Orc,
        Self::Abyssal,
        Self::Celestial,
        Self::DeepSpeech,
        Self::Druidic,
        Self::Infernal,
        Self::Primordial,
        Self::Sylvan,
        Self::ThievesCant,
        Self::Undercommon,
    ];

    /// Canonical lowercase `snake_case` ruleset identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Common => "common",
            Self::CommonSignLanguage => "common_sign_language",
            Self::Draconic => "draconic",
            Self::Dwarvish => "dwarvish",
            Self::Elvish => "elvish",
            Self::Giant => "giant",
            Self::Gnomish => "gnomish",
            Self::Goblin => "goblin",
            Self::Halfling => "halfling",
            Self::Orc => "orc",
            Self::Abyssal => "abyssal",
            Self::Celestial => "celestial",
            Self::DeepSpeech => "deep_speech",
            Self::Druidic => "druidic",
            Self::Infernal => "infernal",
            Self::Primordial => "primordial",
            Self::Sylvan => "sylvan",
            Self::ThievesCant => "thieves_cant",
            Self::Undercommon => "undercommon",
        }
    }
}

/// Saving throw, skill, equipment, weapon mastery, tool, and language proficiencies.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Proficiencies {
    /// Proficient saving throw abilities.
    pub saving_throws: BTreeSet<Ability>,
    /// Trained skills and their proficiency tier.
    pub skills: BTreeMap<Skill, ProficiencyLevel>,
    /// Armor training categories.
    pub armor: BTreeSet<ArmorCategory>,
    /// Broad weapon categories (`Simple`, `Martial`).
    pub weapons: BTreeSet<WeaponCategory>,
    /// Individual proficient weapon catalog IDs (when not granted by a full category).
    pub weapon_ids: BTreeSet<CatalogId>,
    /// 2024 5e Weapon Mastery catalog weapon IDs currently prepared.
    pub weapon_masteries: BTreeSet<CatalogId>,
    /// Proficient tool catalog IDs.
    pub tools: BTreeSet<CatalogId>,
    /// Known languages.
    pub languages: BTreeSet<LanguageId>,
}

impl Proficiencies {
    /// Total modifier for `skill`, combining effective ability modifier, proficiency tier,
    /// active skill bonuses, and 2024 exhaustion penalty (`-2 * level`).
    pub fn skill_modifier(
        &self,
        skill: Skill,
        abilities: AbilityScores,
        proficiency_bonus: u8,
        conditions: Option<&Conditions>,
        effects: Option<&ActiveEffects>,
    ) -> i16 {
        let ability_mod = i16::from(abilities.modifier(skill.ability(), effects));
        let prof = match self.skills.get(&skill) {
            Some(ProficiencyLevel::Half) => i16::from(proficiency_bonus / 2),
            Some(ProficiencyLevel::Proficient) => i16::from(proficiency_bonus),
            Some(ProficiencyLevel::Expertise) => i16::from(proficiency_bonus) * 2,
            None => 0,
        };
        let bonus = effects.map_or(0, |e| e.flat_skill_bonus(skill));
        let exhaustion = i16::from(conditions.map_or(0, Conditions::d20_test_penalty));
        ability_mod + prof + bonus + exhaustion
    }

    /// Derived passive score (`10 + skill_modifier`), such as Passive Perception.
    pub fn passive_score(
        &self,
        skill: Skill,
        abilities: AbilityScores,
        proficiency_bonus: u8,
        conditions: Option<&Conditions>,
        effects: Option<&ActiveEffects>,
    ) -> i16 {
        10 + self.skill_modifier(skill, abilities, proficiency_bonus, conditions, effects)
    }
}

// ---------------------------------------------------------------------------
// Group 3: Character Identity & Progression
// ---------------------------------------------------------------------------

/// The 9 playable species in 2024 SRD 5.2.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeciesId {
    /// Dragonborn.
    Dragonborn,
    /// Dwarf.
    Dwarf,
    /// Elf.
    Elf,
    /// Gnome.
    Gnome,
    /// Goliath.
    Goliath,
    /// Halfling.
    Halfling,
    /// Human.
    Human,
    /// Orc.
    Orc,
    /// Tiefling.
    Tiefling,
}

impl SpeciesId {
    /// All 9 SRD 5.2.1 species.
    pub const ALL: [Self; 9] = [
        Self::Dragonborn,
        Self::Dwarf,
        Self::Elf,
        Self::Gnome,
        Self::Goliath,
        Self::Halfling,
        Self::Human,
        Self::Orc,
        Self::Tiefling,
    ];

    /// Canonical lowercase ruleset identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dragonborn => "dragonborn",
            Self::Dwarf => "dwarf",
            Self::Elf => "elf",
            Self::Gnome => "gnome",
            Self::Goliath => "goliath",
            Self::Halfling => "halfling",
            Self::Human => "human",
            Self::Orc => "orc",
            Self::Tiefling => "tiefling",
        }
    }
}

/// Species component attached to a character or humanoid NPC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Species {
    /// Fixed SRD 5.2.1 species identifier.
    pub id: SpeciesId,
}

/// The 4 starting backgrounds in 2024 SRD 5.2.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundId {
    /// Acolyte.
    Acolyte,
    /// Criminal.
    Criminal,
    /// Sage.
    Sage,
    /// Soldier.
    Soldier,
}

impl BackgroundId {
    /// All 4 SRD 5.2.1 backgrounds.
    pub const ALL: [Self; 4] = [Self::Acolyte, Self::Criminal, Self::Sage, Self::Soldier];

    /// Canonical lowercase ruleset identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Acolyte => "acolyte",
            Self::Criminal => "criminal",
            Self::Sage => "sage",
            Self::Soldier => "soldier",
        }
    }
}

/// Background component attached to a character.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Background {
    /// Fixed SRD 5.2.1 background identifier.
    pub id: BackgroundId,
}

/// The 12 character classes in 2024 SRD 5.2.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassId {
    /// Barbarian (d12).
    Barbarian,
    /// Bard (d8).
    Bard,
    /// Cleric (d8).
    Cleric,
    /// Druid (d8).
    Druid,
    /// Fighter (d10).
    Fighter,
    /// Monk (d8).
    Monk,
    /// Paladin (d10).
    Paladin,
    /// Ranger (d10).
    Ranger,
    /// Rogue (d8).
    Rogue,
    /// Sorcerer (d6).
    Sorcerer,
    /// Warlock (d8).
    Warlock,
    /// Wizard (d6).
    Wizard,
}

impl ClassId {
    /// All 12 SRD 5.2.1 classes in alphabetical order.
    pub const ALL: [Self; 12] = [
        Self::Barbarian,
        Self::Bard,
        Self::Cleric,
        Self::Druid,
        Self::Fighter,
        Self::Monk,
        Self::Paladin,
        Self::Ranger,
        Self::Rogue,
        Self::Sorcerer,
        Self::Warlock,
        Self::Wizard,
    ];

    /// Canonical lowercase ruleset identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Barbarian => "barbarian",
            Self::Bard => "bard",
            Self::Cleric => "cleric",
            Self::Druid => "druid",
            Self::Fighter => "fighter",
            Self::Monk => "monk",
            Self::Paladin => "paladin",
            Self::Ranger => "ranger",
            Self::Rogue => "rogue",
            Self::Sorcerer => "sorcerer",
            Self::Warlock => "warlock",
            Self::Wizard => "wizard",
        }
    }

    /// Hit die sides granted per level in this class.
    pub const fn hit_die_sides(self) -> u8 {
        match self {
            Self::Barbarian => 12,
            Self::Fighter | Self::Paladin | Self::Ranger => 10,
            Self::Bard | Self::Cleric | Self::Druid | Self::Monk | Self::Rogue | Self::Warlock => 8,
            Self::Sorcerer | Self::Wizard => 6,
        }
    }
}

/// The 12 subclasses in 2024 SRD 5.2.1 (1 per class).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubclassId {
    /// Path of the Berserker (Barbarian).
    PathOfTheBerserker,
    /// College of Lore (Bard).
    CollegeOfLore,
    /// Life Domain (Cleric).
    LifeDomain,
    /// Circle of the Land (Druid).
    CircleOfTheLand,
    /// Champion (Fighter).
    Champion,
    /// Warrior of the Open Hand (Monk).
    WarriorOfTheOpenHand,
    /// Oath of Devotion (Paladin).
    OathOfDevotion,
    /// Hunter (Ranger).
    Hunter,
    /// Thief (Rogue).
    Thief,
    /// Draconic Sorcery (Sorcerer).
    DraconicSorcery,
    /// Fiend Patron (Warlock).
    FiendPatron,
    /// Evoker (Wizard).
    Evoker,
}

impl SubclassId {
    /// All 12 SRD 5.2.1 subclasses.
    pub const ALL: [Self; 12] = [
        Self::PathOfTheBerserker,
        Self::CollegeOfLore,
        Self::LifeDomain,
        Self::CircleOfTheLand,
        Self::Champion,
        Self::WarriorOfTheOpenHand,
        Self::OathOfDevotion,
        Self::Hunter,
        Self::Thief,
        Self::DraconicSorcery,
        Self::FiendPatron,
        Self::Evoker,
    ];

    /// Canonical lowercase `snake_case` ruleset identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PathOfTheBerserker => "path_of_the_berserker",
            Self::CollegeOfLore => "college_of_lore",
            Self::LifeDomain => "life_domain",
            Self::CircleOfTheLand => "circle_of_the_land",
            Self::Champion => "champion",
            Self::WarriorOfTheOpenHand => "warrior_of_the_open_hand",
            Self::OathOfDevotion => "oath_of_devotion",
            Self::Hunter => "hunter",
            Self::Thief => "thief",
            Self::DraconicSorcery => "draconic_sorcery",
            Self::FiendPatron => "fiend_patron",
            Self::Evoker => "evoker",
        }
    }

    /// The single [`ClassId`] this subclass belongs to.
    pub const fn parent_class(self) -> ClassId {
        match self {
            Self::PathOfTheBerserker => ClassId::Barbarian,
            Self::CollegeOfLore => ClassId::Bard,
            Self::LifeDomain => ClassId::Cleric,
            Self::CircleOfTheLand => ClassId::Druid,
            Self::Champion => ClassId::Fighter,
            Self::WarriorOfTheOpenHand => ClassId::Monk,
            Self::OathOfDevotion => ClassId::Paladin,
            Self::Hunter => ClassId::Ranger,
            Self::Thief => ClassId::Rogue,
            Self::DraconicSorcery => ClassId::Sorcerer,
            Self::FiendPatron => ClassId::Warlock,
            Self::Evoker => ClassId::Wizard,
        }
    }
}

/// Levels held in 1 class plus optional subclass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassLevelEntry {
    /// Class identifier.
    pub class: ClassId,
    /// Levels in this class (`1..=20`).
    pub level: u8,
    /// Chosen subclass once unlocked (level 3 in 2024 5e).
    pub subclass: Option<SubclassId>,
}

/// Remaining and maximum hit dice of 1 die size (supports multiclass mixed hit dice).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HitDiePool {
    /// Die sides (`6`, `8`, `10`, or `12`).
    pub die_sides: u8,
    /// Unspent hit dice remaining.
    pub current: u8,
    /// Total hit dice of this size.
    pub max: u8,
}

/// Multiclass-ready class progression and hit dice pools for a character.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct ClassLevels {
    /// Ordered class entries (primary class first).
    pub entries: Vec<ClassLevelEntry>,
    /// Hit dice pools grouped by die size.
    pub hit_dice: Vec<HitDiePool>,
}

impl ClassLevels {
    /// Creates a single-class character at `level` (`1..=20`) with full hit dice.
    pub fn single(class: ClassId, level: u8, subclass: Option<SubclassId>) -> Self {
        let clamped = level.clamp(1, MAX_CHARACTER_LEVEL);
        Self {
            entries: vec![ClassLevelEntry {
                class,
                level: clamped,
                subclass,
            }],
            hit_dice: vec![HitDiePool {
                die_sides: class.hit_die_sides(),
                current: clamped,
                max: clamped,
            }],
        }
    }

    /// Sum of levels across all classes.
    pub fn total_level(&self) -> u8 {
        self.entries
            .iter()
            .fold(0_u8, |acc, entry| acc.saturating_add(entry.level))
    }

    /// Standard 5e proficiency bonus (`+2` at levels 1–4 up to `+6` at levels 17–20).
    pub fn proficiency_bonus(&self) -> u8 {
        let level = self.total_level().max(1);
        2 + (level - 1) / 4
    }

    /// Adds 1 level in `class` (and records `subclass` if provided), updating hit dice pools.
    pub fn add_level(&mut self, class: ClassId, subclass: Option<SubclassId>) -> bool {
        if self.total_level() >= MAX_CHARACTER_LEVEL {
            return false;
        }
        if let Some(sub) = subclass
            && sub.parent_class() != class
        {
            return false;
        }
        if let Some(entry) = self.entries.iter_mut().find(|e| e.class == class) {
            entry.level = entry.level.saturating_add(1);
            if subclass.is_some() {
                entry.subclass = subclass;
            }
        } else {
            self.entries.push(ClassLevelEntry {
                class,
                level: 1,
                subclass,
            });
        }
        let sides = class.hit_die_sides();
        if let Some(pool) = self.hit_dice.iter_mut().find(|p| p.die_sides == sides) {
            pool.max = pool.max.saturating_add(1);
            pool.current = pool.current.saturating_add(1);
        } else {
            self.hit_dice.push(HitDiePool {
                die_sides: sides,
                current: 1,
                max: 1,
            });
        }
        true
    }
}

/// Closed set of 2024 SRD 5.2.1 feats (Origin, General, Fighting Style, and Epic Boons).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatId {
    /// Alert (Origin).
    Alert,
    /// Magic Initiate (Origin).
    MagicInitiate,
    /// Savage Attacker (Origin).
    SavageAttacker,
    /// Skilled (Origin).
    Skilled,
    /// Ability Score Improvement (General).
    AbilityScoreImprovement,
    /// Grappler (General).
    Grappler,
    /// Archery (Fighting Style).
    Archery,
    /// Defense (Fighting Style).
    Defense,
    /// Great Weapon Fighting (Fighting Style).
    GreatWeaponFighting,
    /// Two-Weapon Fighting (Fighting Style).
    TwoWeaponFighting,
    /// Boon of Combat Prowess (Epic Boon).
    BoonOfCombatProwess,
    /// Boon of Dimensional Travel (Epic Boon).
    BoonOfDimensionalTravel,
    /// Boon of Fate (Epic Boon).
    BoonOfFate,
    /// Boon of Irresistible Offense (Epic Boon).
    BoonOfIrresistibleOffense,
    /// Boon of Spell Recall (Epic Boon).
    BoonOfSpellRecall,
    /// Boon of the Night Spirit (Epic Boon).
    BoonOfTheNightSpirit,
    /// Boon of Truesight (Epic Boon).
    BoonOfTruesight,
}

impl FeatId {
    /// All 17 SRD 5.2.1 feats.
    pub const ALL: [Self; 17] = [
        Self::Alert,
        Self::MagicInitiate,
        Self::SavageAttacker,
        Self::Skilled,
        Self::AbilityScoreImprovement,
        Self::Grappler,
        Self::Archery,
        Self::Defense,
        Self::GreatWeaponFighting,
        Self::TwoWeaponFighting,
        Self::BoonOfCombatProwess,
        Self::BoonOfDimensionalTravel,
        Self::BoonOfFate,
        Self::BoonOfIrresistibleOffense,
        Self::BoonOfSpellRecall,
        Self::BoonOfTheNightSpirit,
        Self::BoonOfTruesight,
    ];

    /// Canonical lowercase `snake_case` ruleset identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alert => "alert",
            Self::MagicInitiate => "magic_initiate",
            Self::SavageAttacker => "savage_attacker",
            Self::Skilled => "skilled",
            Self::AbilityScoreImprovement => "ability_score_improvement",
            Self::Grappler => "grappler",
            Self::Archery => "archery",
            Self::Defense => "defense",
            Self::GreatWeaponFighting => "great_weapon_fighting",
            Self::TwoWeaponFighting => "two_weapon_fighting",
            Self::BoonOfCombatProwess => "boon_of_combat_prowess",
            Self::BoonOfDimensionalTravel => "boon_of_dimensional_travel",
            Self::BoonOfFate => "boon_of_fate",
            Self::BoonOfIrresistibleOffense => "boon_of_irresistible_offense",
            Self::BoonOfSpellRecall => "boon_of_spell_recall",
            Self::BoonOfTheNightSpirit => "boon_of_the_night_spirit",
            Self::BoonOfTruesight => "boon_of_truesight",
        }
    }
}

/// Class features, feats, magic item attunement, and 2024 Heroic Inspiration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct CharacterTraits {
    /// Validated ruleset identifiers for unlocked class, subclass, and species features.
    pub features: BTreeSet<RulesetId>,
    /// Feats taken by this character.
    pub feats: BTreeSet<FeatId>,
    /// Attuned inventory stack identifiers (at most [`MAX_ATTUNED_ITEMS`] = 3).
    pub attuned_items: Vec<StackId>,
    /// Whether the creature currently holds Heroic Inspiration (reroll 1 d20 Test).
    pub heroic_inspiration: bool,
}

/// Where a spell entry comes from (supports multiclass, species innate, feat, and item spells).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpellSourceKind {
    /// Granted by a class (see `SpellEntry::source_class`).
    Class,
    /// Granted by species trait (innate spellcasting).
    Species,
    /// Granted by a feat (such as Magic Initiate).
    Feat,
    /// Cast from an equipped or carried item (see `SpellEntry::source_item`).
    Item,
}

/// Preparation state of a spell entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpellPrepState {
    /// Cantrip (at-will, level 0).
    Cantrip,
    /// Prepared spell using standard spell slots.
    Prepared,
    /// Always-prepared spell (such as Domain or Oath spells) that does not count against limit.
    AlwaysPrepared,
    /// Known in spellbook/list but not currently prepared.
    KnownUnprepared,
    /// Innate or item-granted spell cast via a feature or item charge pool (or at-will).
    Innate,
}

/// 1 spell known, prepared, innate, or granted by an item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpellEntry {
    /// Validated spell identifier in the ruleset.
    pub spell_id: RulesetId,
    /// Spell circle (`0` for cantrip, `1..=9` for leveled spells).
    pub spell_level: u8,
    /// Where this spell comes from.
    pub source: SpellSourceKind,
    /// Owning class when `source == SpellSourceKind::Class`.
    pub source_class: Option<ClassId>,
    /// Owning inventory stack when `source == SpellSourceKind::Item`.
    pub source_item: Option<StackId>,
    /// Spellcasting ability used to compute save DC and attack bonus, if ability-based.
    pub casting_ability: Option<Ability>,
    /// Fixed save DC (such as magic items or scrolls), overriding ability-derived DC when set.
    pub fixed_dc: Option<u8>,
    /// Preparation state.
    pub prep_state: SpellPrepState,
    /// Charge or feature-use cost when cast from an item or innate pool (`0` = at-will or slots).
    pub charge_cost: u8,
}

/// All spells accessible to a creature across classes, species, feats, and magic items.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Spellbook {
    /// All spell entries known, prepared, innate, or item-granted.
    pub entries: Vec<SpellEntry>,
}

impl Spellbook {
    /// Derived spell save DC (`8 + proficiency_bonus + ability_mod`) or `fixed_dc` for `entry`.
    pub fn spell_save_dc(
        entry: &SpellEntry,
        abilities: AbilityScores,
        proficiency_bonus: u8,
        effects: Option<&ActiveEffects>,
    ) -> Option<u8> {
        if let Some(dc) = entry.fixed_dc {
            return Some(dc);
        }
        let ability = entry.casting_ability?;
        let total =
            8_i16 + i16::from(proficiency_bonus) + i16::from(abilities.modifier(ability, effects));
        u8::try_from(total.clamp(1, 255)).ok()
    }
}

/// Standard 5e coinage carried by any character, chest, loot pile, or shopkeeper.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct CoinPurse {
    /// Copper pieces (`1 cp`).
    pub cp: u32,
    /// Silver pieces (`10 cp`).
    pub sp: u32,
    /// Electrum pieces (`50 cp`).
    pub ep: u32,
    /// Gold pieces (`100 cp`).
    pub gp: u32,
    /// Platinum pieces (`1000 cp`).
    pub pp: u32,
}

impl CoinPurse {
    /// Total value expressed in copper pieces.
    pub fn total_copper(self) -> u64 {
        u64::from(self.cp)
            + u64::from(self.sp) * 10
            + u64::from(self.ep) * 50
            + u64::from(self.gp) * 100
            + u64::from(self.pp) * 1000
    }
}

/// Roleplay bio notes read by the AI DM (no 9-box alignment; Zeta uses the Twill).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct CharacterBio {
    /// Personality traits.
    pub traits: String,
    /// Guiding ideals.
    pub ideals: String,
    /// Personal bonds.
    pub bonds: String,
    /// Character flaws or lasting afflictions.
    pub flaws: String,
}

// ---------------------------------------------------------------------------
// Group 4: Dynamic Status Effects, Conditions, Hazards, Concentration & Resources
// ---------------------------------------------------------------------------

/// The 15 official conditions in 2024 SRD 5.2.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionId {
    /// Blinded.
    Blinded,
    /// Charmed.
    Charmed,
    /// Deafened.
    Deafened,
    /// Exhaustion (level tracked in `Conditions::exhaustion_level`).
    Exhaustion,
    /// Frightened.
    Frightened,
    /// Grappled.
    Grappled,
    /// Incapacitated.
    Incapacitated,
    /// Invisible.
    Invisible,
    /// Paralyzed.
    Paralyzed,
    /// Petrified.
    Petrified,
    /// Poisoned.
    Poisoned,
    /// Prone.
    Prone,
    /// Restrained.
    Restrained,
    /// Stunned.
    Stunned,
    /// Unconscious.
    Unconscious,
}

impl ConditionId {
    /// All 15 SRD 5.2.1 conditions in alphabetical order.
    pub const ALL: [Self; 15] = [
        Self::Blinded,
        Self::Charmed,
        Self::Deafened,
        Self::Exhaustion,
        Self::Frightened,
        Self::Grappled,
        Self::Incapacitated,
        Self::Invisible,
        Self::Paralyzed,
        Self::Petrified,
        Self::Poisoned,
        Self::Prone,
        Self::Restrained,
        Self::Stunned,
        Self::Unconscious,
    ];

    /// Canonical lowercase ruleset identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blinded => "blinded",
            Self::Charmed => "charmed",
            Self::Deafened => "deafened",
            Self::Exhaustion => "exhaustion",
            Self::Frightened => "frightened",
            Self::Grappled => "grappled",
            Self::Incapacitated => "incapacitated",
            Self::Invisible => "invisible",
            Self::Paralyzed => "paralyzed",
            Self::Petrified => "petrified",
            Self::Poisoned => "poisoned",
            Self::Prone => "prone",
            Self::Restrained => "restrained",
            Self::Stunned => "stunned",
            Self::Unconscious => "unconscious",
        }
    }

    /// Whether gaining this condition implies `Incapacitated` (which also breaks Concentration).
    pub const fn breaks_concentration(self) -> bool {
        matches!(
            self,
            Self::Incapacitated
                | Self::Paralyzed
                | Self::Petrified
                | Self::Stunned
                | Self::Unconscious
        )
    }
}

/// Environmental hazards tracked in 2024 SRD 5.2.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HazardId {
    /// Burning (1d4 Fire at start of turn until extinguished).
    Burning,
    /// Dehydration (insufficient water).
    Dehydration,
    /// Malnutrition (insufficient food).
    Malnutrition,
    /// Suffocation (running out of breathable air).
    Suffocation,
}

impl HazardId {
    /// All 4 SRD 5.2.1 ongoing hazards.
    pub const ALL: [Self; 4] = [
        Self::Burning,
        Self::Dehydration,
        Self::Malnutrition,
        Self::Suffocation,
    ];

    /// Canonical lowercase ruleset identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Burning => "burning",
            Self::Dehydration => "dehydration",
            Self::Malnutrition => "malnutrition",
            Self::Suffocation => "suffocation",
        }
    }
}

/// Active conditions, 2024 exhaustion level (`0..=6`), and ongoing hazards on an entity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Conditions {
    /// Active SRD 5.2.1 conditions.
    pub active: BTreeSet<ConditionId>,
    /// Exhaustion level (`0..=6`). Kept synchronized with `ConditionId::Exhaustion`.
    pub exhaustion_level: u8,
    /// Ongoing environmental hazards.
    pub hazards: BTreeSet<HazardId>,
}

impl Conditions {
    /// 2024 SRD 5.2.1 exhaustion penalty on all D20 Tests (`-2 * exhaustion_level`).
    pub const fn d20_test_penalty(&self) -> i8 {
        let clamped = if self.exhaustion_level > MAX_EXHAUSTION_LEVEL {
            MAX_EXHAUSTION_LEVEL
        } else {
            self.exhaustion_level
        };
        -(clamped * 2).cast_signed()
    }

    /// 2024 SRD 5.2.1 exhaustion speed reduction in feet (`5 * exhaustion_level`).
    pub const fn speed_penalty_ft(&self) -> u16 {
        let clamped = if self.exhaustion_level > MAX_EXHAUSTION_LEVEL {
            MAX_EXHAUSTION_LEVEL
        } else {
            self.exhaustion_level
        };
        (clamped as u16) * 5
    }

    /// Whether any active condition forces speed to `0` (`Grappled`, `Paralyzed`, `Petrified`,
    /// `Restrained`, `Unconscious`). Note: in 2024 5e, `Stunned` no longer zeroes speed.
    pub fn zeros_speed(&self) -> bool {
        self.active.iter().any(|condition| {
            matches!(
                condition,
                ConditionId::Grappled
                    | ConditionId::Paralyzed
                    | ConditionId::Petrified
                    | ConditionId::Restrained
                    | ConditionId::Unconscious
            )
        })
    }

    /// Applies condition additions/removals, exhaustion delta, and hazards, respecting
    /// `immunities`. Returns `true` if a concentration-breaking condition was added.
    pub fn apply_changes(
        &mut self,
        add: &[ConditionId],
        remove: &[ConditionId],
        exhaustion_delta: i8,
        add_hazards: &[HazardId],
        remove_hazards: &[HazardId],
        immunities: Option<&ConditionImmunities>,
    ) -> bool {
        let mut broke_concentration = false;
        for &cond in remove {
            self.active.remove(&cond);
            if cond == ConditionId::Exhaustion {
                self.exhaustion_level = 0;
            }
        }
        for &cond in add {
            if immunities.is_some_and(|im| im.immune.contains(&cond)) {
                continue;
            }
            self.active.insert(cond);
            if cond == ConditionId::Exhaustion && self.exhaustion_level == 0 {
                self.exhaustion_level = 1;
            }
            if cond.breaks_concentration() {
                broke_concentration = true;
            }
        }
        if exhaustion_delta != 0
            && !immunities.is_some_and(|im| im.immune.contains(&ConditionId::Exhaustion))
        {
            let next = (i16::from(self.exhaustion_level) + i16::from(exhaustion_delta))
                .clamp(0, i16::from(MAX_EXHAUSTION_LEVEL));
            self.exhaustion_level = u8::try_from(next).unwrap_or(0);
            if self.exhaustion_level > 0 {
                self.active.insert(ConditionId::Exhaustion);
            } else {
                self.active.remove(&ConditionId::Exhaustion);
            }
        }
        for &hazard in remove_hazards {
            self.hazards.remove(&hazard);
        }
        for &hazard in add_hazards {
            self.hazards.insert(hazard);
        }
        broke_concentration
    }
}

/// Death saving throw progress for a creature at 0 hit points.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct DeathSaves {
    /// Successful death saves (`0..=3`).
    pub successes: u8,
    /// Failed death saves (`0..=3`).
    pub failures: u8,
    /// Whether the creature is stable at 0 hit points.
    pub stabilized: bool,
}

impl DeathSaves {
    /// Resets death saves when healed above 0 hit points.
    pub fn reset(&mut self) {
        self.successes = 0;
        self.failures = 0;
        self.stabilized = false;
    }
}

/// Single active Concentration slot on a spellcaster (5e allows at most 1 at a time).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Concentration {
    /// Effect currently being concentrated on, or `None`.
    pub active_effect_id: Option<String>,
    /// Ruleset identifier of the spell or feature being concentrated on.
    pub source_id: Option<RulesetId>,
    /// Remaining duration in rounds, if tracked.
    pub remaining_rounds: Option<u32>,
}

impl Concentration {
    /// Clears active concentration.
    pub fn clear(&mut self) {
        self.active_effect_id = None;
        self.source_id = None;
        self.remaining_rounds = None;
    }
}

/// Which category of rolls a [`StatModifier`] applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollScope {
    /// Every D20 Test (attack rolls, ability checks, saving throws).
    AllD20Tests,
    /// Attack rolls.
    AttackRolls,
    /// Ability checks (including skill checks).
    AbilityChecks,
    /// Saving throws.
    SavingThrows,
    /// Saving throws against spells and magical effects (e.g. Magic Resistance).
    SavingThrowsVsMagic,
    /// Constitution saving throws to maintain Concentration.
    ConcentrationSaves,
    /// Initiative rolls.
    Initiative,
    /// Death saving throws.
    DeathSaves,
}

/// Discriminator for a flat, postcard-safe [`StatModifier`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatModifierKind {
    /// Flat bonus or penalty to Armor Class (`value`).
    AcBonus,
    /// Flat bonus or penalty to walking speed in feet (`value`).
    SpeedBonusFt,
    /// Flat and/or dice bonus to rolls in `roll_scope` (optional `ability`/`skill` filter).
    RollBonus,
    /// Grants Advantage on `roll_scope` (optional `ability`/`skill` filter).
    Advantage,
    /// Imposes Disadvantage on `roll_scope` (optional `ability`/`skill` filter).
    Disadvantage,
    /// Sets a minimum effective ability score (`ability`, `value`, e.g. Belt of Giant Strength).
    AbilityScoreMin,
    /// Adds a flat bonus or penalty to an ability score (`ability`, `value`).
    AbilityScoreBonus,
    /// Grants temporary damage resistance (`damage_type`).
    DamageResistance,
    /// Grants temporary damage immunity (`damage_type`).
    DamageImmunity,
    /// Imposes temporary damage vulnerability (`damage_type`).
    DamageVulnerability,
}

/// 1 structured mechanical modifier inside an [`ActiveEffect`].
///
/// Flat struct (no internally tagged enum) so both JSON and `postcard` serialize it identically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatModifier {
    /// What this modifier changes.
    pub kind: StatModifierKind,
    /// Flat numeric bonus, penalty, or minimum score (`0` when unused).
    #[serde(default)]
    pub value: i16,
    /// Optional bonus or penalty dice (such as `+1d4` for Bless or Guidance; negative `value`
    /// sign or `dice_negative: true` indicates subtraction such as Bane).
    #[serde(default)]
    pub dice: Option<Dice>,
    /// When `true`, `dice` is subtracted rather than added (such as Bane's `-1d4`).
    #[serde(default)]
    pub dice_negative: bool,
    /// Optional roll category scope.
    #[serde(default)]
    pub roll_scope: Option<RollScope>,
    /// Optional ability filter (such as Dexterity saves for Shield Master, or STR for a Belt).
    #[serde(default)]
    pub ability: Option<Ability>,
    /// Optional skill filter.
    #[serde(default)]
    pub skill: Option<Skill>,
    /// Optional damage type (for resistance/immunity/vulnerability modifiers).
    #[serde(default)]
    pub damage_type: Option<DamageType>,
}

/// 1 active timed or untimed buff, debuff, stance, or cover state (Bless, Rage, Shield, Dodge, Cover).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveEffect {
    /// Unique identifier on this entity (e.g. `"bless"`, `"rage"`, `"half_cover"`).
    pub id: String,
    /// Display label shown to players and the AI DM.
    pub name: String,
    /// Optional ruleset identifier linking back to the SRD spell/feature definition.
    #[serde(default)]
    pub source: Option<RulesetId>,
    /// Remaining rounds (`None` = until dismissed, unequipped, or rest).
    #[serde(default)]
    pub duration_rounds: Option<u32>,
    /// Structured modifiers granted while this effect is active.
    #[serde(default)]
    pub modifiers: Vec<StatModifier>,
}

/// All active temporary buffs, debuffs, stances, magic-item passive effects, and cover on an entity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct ActiveEffects {
    /// Active effects in insertion order.
    pub effects: Vec<ActiveEffect>,
}

impl ActiveEffects {
    /// Inserts or replaces an effect with the same `id`.
    pub fn upsert(&mut self, effect: ActiveEffect) {
        if let Some(existing) = self.effects.iter_mut().find(|e| e.id == effect.id) {
            *existing = effect;
        } else {
            self.effects.push(effect);
        }
    }

    /// Removes the effect with `effect_id`, returning `true` if one was removed.
    pub fn remove(&mut self, effect_id: &str) -> bool {
        let before = self.effects.len();
        self.effects.retain(|e| e.id != effect_id);
        self.effects.len() < before
    }

    /// Sum of all flat AC bonuses across active effects.
    pub fn total_ac_bonus(&self) -> i16 {
        self.effects
            .iter()
            .flat_map(|e| &e.modifiers)
            .filter(|m| m.kind == StatModifierKind::AcBonus)
            .map(|m| m.value)
            .sum()
    }

    /// Sum of all flat walking speed bonuses in feet across active effects.
    pub fn total_speed_bonus_ft(&self) -> i32 {
        self.effects
            .iter()
            .flat_map(|e| &e.modifiers)
            .filter(|m| m.kind == StatModifierKind::SpeedBonusFt)
            .map(|m| i32::from(m.value))
            .sum()
    }

    /// Sum of flat skill bonuses matching `skill` (or unscoped `AbilityChecks` / `AllD20Tests`).
    pub fn flat_skill_bonus(&self, skill: Skill) -> i16 {
        self.effects
            .iter()
            .flat_map(|e| &e.modifiers)
            .filter(|m| {
                m.kind == StatModifierKind::RollBonus
                    && matches!(
                        m.roll_scope,
                        Some(RollScope::AbilityChecks | RollScope::AllD20Tests) | None
                    )
                    && (m.skill.is_none() || m.skill == Some(skill))
                    && (m.ability.is_none() || m.ability == Some(skill.ability()))
            })
            .map(|m| m.value)
            .sum()
    }

    /// Whether any active effect grants temporary resistance to `damage_type`.
    pub fn has_damage_resistance(&self, damage_type: DamageType) -> bool {
        self.effects.iter().flat_map(|e| &e.modifiers).any(|m| {
            m.kind == StatModifierKind::DamageResistance && m.damage_type == Some(damage_type)
        })
    }

    /// Whether any active effect grants temporary immunity to `damage_type`.
    pub fn has_damage_immunity(&self, damage_type: DamageType) -> bool {
        self.effects.iter().flat_map(|e| &e.modifiers).any(|m| {
            m.kind == StatModifierKind::DamageImmunity && m.damage_type == Some(damage_type)
        })
    }

    /// Whether any active effect imposes temporary vulnerability to `damage_type`.
    pub fn has_damage_vulnerability(&self, damage_type: DamageType) -> bool {
        self.effects.iter().flat_map(|e| &e.modifiers).any(|m| {
            m.kind == StatModifierKind::DamageVulnerability && m.damage_type == Some(damage_type)
        })
    }
}

/// When a finite resource pool recovers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RechargeSchedule {
    /// Recovers on a Short or Long Rest.
    ShortRest,
    /// Recovers on a Long Rest.
    LongRest,
    /// Recovers at dawn (common for magic items).
    Dawn,
    /// Recovers at the start of the creature's turn (such as Legendary Actions).
    TurnStart,
    /// Monster ability recharge on a 5 or 6 on 1d6.
    Recharge56,
    /// Monster ability recharge on a 6 on 1d6.
    Recharge6,
}

/// Closed set of SRD 5.2.1 named class and monster resource pools.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceId {
    /// Barbarian Rage uses.
    Rage,
    /// Bard Bardic Inspiration uses.
    BardicInspiration,
    /// Cleric / Paladin Channel Divinity uses.
    ChannelDivinity,
    /// Druid Wild Shape uses.
    WildShape,
    /// Fighter Second Wind uses.
    SecondWind,
    /// Fighter Action Surge uses.
    ActionSurge,
    /// Fighter Indomitable uses.
    Indomitable,
    /// Monk Focus Points.
    FocusPoints,
    /// Paladin Lay on Hands hit point pool.
    LayOnHands,
    /// Ranger Favored Enemy free Hunter's Mark casts.
    FavoredEnemy,
    /// Sorcerer Sorcery Points.
    SorceryPoints,
    /// Wizard Arcane Recovery use.
    ArcaneRecovery,
    /// Monster Legendary Resistance uses.
    LegendaryResistance,
    /// Monster Legendary Action points.
    LegendaryActions,
    /// Monster breath weapon or recharge trait use.
    RechargeTrait,
}

impl ResourceId {
    /// All 15 SRD 5.2.1 named resource pools.
    pub const ALL: [Self; 15] = [
        Self::Rage,
        Self::BardicInspiration,
        Self::ChannelDivinity,
        Self::WildShape,
        Self::SecondWind,
        Self::ActionSurge,
        Self::Indomitable,
        Self::FocusPoints,
        Self::LayOnHands,
        Self::FavoredEnemy,
        Self::SorceryPoints,
        Self::ArcaneRecovery,
        Self::LegendaryResistance,
        Self::LegendaryActions,
        Self::RechargeTrait,
    ];

    /// Canonical lowercase `snake_case` ruleset identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rage => "rage",
            Self::BardicInspiration => "bardic_inspiration",
            Self::ChannelDivinity => "channel_divinity",
            Self::WildShape => "wild_shape",
            Self::SecondWind => "second_wind",
            Self::ActionSurge => "action_surge",
            Self::Indomitable => "indomitable",
            Self::FocusPoints => "focus_points",
            Self::LayOnHands => "lay_on_hands",
            Self::FavoredEnemy => "favored_enemy",
            Self::SorceryPoints => "sorcery_points",
            Self::ArcaneRecovery => "arcane_recovery",
            Self::LegendaryResistance => "legendary_resistance",
            Self::LegendaryActions => "legendary_actions",
            Self::RechargeTrait => "recharge_trait",
        }
    }
}

/// 1 spell slot level (`1..=9`) counter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotPool {
    /// Unspent slots at this spell level.
    pub current: u8,
    /// Maximum slots at this spell level.
    pub max: u8,
}

/// Warlock Pact Magic slots (all cast at 1 shared slot level, recharge on Short Rest).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PactSlotPool {
    /// Spell level of every pact slot (`1..=5`).
    pub slot_level: u8,
    /// Unspent pact slots.
    pub current: u8,
    /// Maximum pact slots.
    pub max: u8,
}

/// Generic finite pool (`current / max`) with a recharge schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourcePool {
    /// Remaining uses or charges.
    pub current: u16,
    /// Maximum uses or charges.
    pub max: u16,
    /// When this pool recovers.
    pub recharge: RechargeSchedule,
}

/// Flat selector identifying 1 resource pool to spend or restore (postcard-safe).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceTargetKind {
    /// Standard spell slot level (`spell_level: 1..=9`).
    SpellSlot,
    /// Warlock Pact Magic slot.
    PactSlot,
    /// Named class/monster pool (`resource`).
    ClassPool,
    /// Validated feature/species limited use (`feature_id`).
    FeatureUse,
    /// Magic item charge pool (`stack_id`).
    ItemCharge,
}

/// Identifies a specific pool inside [`ResourcePools`] for `SpendResource` / `RestoreResource`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceTarget {
    /// Which pool family is targeted.
    pub kind: ResourceTargetKind,
    /// Spell level `1..=9` when `kind == SpellSlot`.
    #[serde(default)]
    pub spell_level: Option<u8>,
    /// Class/monster resource when `kind == ClassPool`.
    #[serde(default)]
    pub resource: Option<ResourceId>,
    /// Validated feature ID when `kind == FeatureUse`.
    #[serde(default)]
    pub feature_id: Option<RulesetId>,
    /// Inventory stack ID when `kind == ItemCharge`.
    #[serde(default)]
    pub stack_id: Option<StackId>,
}

/// All finite spell slots, class resources, feature uses, and magic-item charge pools on an entity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct ResourcePools {
    /// Standard spell slots for spell levels 1 through 9 (index `level - 1`).
    pub spell_slots: [SlotPool; 9],
    /// Optional Warlock Pact Magic slots.
    pub pact_slots: Option<PactSlotPool>,
    /// Named class and monster pools (Rage, Second Wind, Focus Points, Legendary Resistance, etc.).
    pub class_pools: BTreeMap<ResourceId, ResourcePool>,
    /// Per-feature or per-species-trait limited uses (such as 1/day innate spells).
    pub feature_uses: BTreeMap<RulesetId, ResourcePool>,
    /// Item-bound charge pools keyed by inventory [`StackId`] (wands, staves, etc.).
    pub item_charges: BTreeMap<StackId, ResourcePool>,
}

impl ResourcePools {
    /// Spends `amount` from `target`. Returns `false` without mutating if missing or insufficient.
    pub fn spend(&mut self, target: &ResourceTarget, amount: u16) -> bool {
        if amount == 0 {
            return false;
        }
        match target.kind {
            ResourceTargetKind::SpellSlot => {
                let Some(level) = target.spell_level.filter(|&l| (1..=9).contains(&l)) else {
                    return false;
                };
                let Ok(amount_u8) = u8::try_from(amount) else {
                    return false;
                };
                let pool = &mut self.spell_slots[usize::from(level - 1)];
                if pool.current < amount_u8 {
                    return false;
                }
                pool.current -= amount_u8;
                true
            }
            ResourceTargetKind::PactSlot => {
                let Some(pool) = self.pact_slots.as_mut() else {
                    return false;
                };
                let Ok(amount_u8) = u8::try_from(amount) else {
                    return false;
                };
                if pool.current < amount_u8 {
                    return false;
                }
                pool.current -= amount_u8;
                true
            }
            ResourceTargetKind::ClassPool => {
                let Some(id) = target.resource else {
                    return false;
                };
                let Some(pool) = self.class_pools.get_mut(&id) else {
                    return false;
                };
                if pool.current < amount {
                    return false;
                }
                pool.current -= amount;
                true
            }
            ResourceTargetKind::FeatureUse => {
                let Some(id) = target.feature_id.as_ref() else {
                    return false;
                };
                let Some(pool) = self.feature_uses.get_mut(id) else {
                    return false;
                };
                if pool.current < amount {
                    return false;
                }
                pool.current -= amount;
                true
            }
            ResourceTargetKind::ItemCharge => {
                let Some(id) = target.stack_id else {
                    return false;
                };
                let Some(pool) = self.item_charges.get_mut(&id) else {
                    return false;
                };
                if pool.current < amount {
                    return false;
                }
                pool.current -= amount;
                true
            }
        }
    }

    /// Restores up to `amount` to `target`, clamped at pool maximum. Returns `false` if missing.
    pub fn restore(&mut self, target: &ResourceTarget, amount: u16) -> bool {
        if amount == 0 {
            return false;
        }
        match target.kind {
            ResourceTargetKind::SpellSlot => {
                let Some(level) = target.spell_level.filter(|&l| (1..=9).contains(&l)) else {
                    return false;
                };
                let amount_u8 = u8::try_from(amount).unwrap_or(u8::MAX);
                let pool = &mut self.spell_slots[usize::from(level - 1)];
                pool.current = pool.current.saturating_add(amount_u8).min(pool.max);
                true
            }
            ResourceTargetKind::PactSlot => {
                let Some(pool) = self.pact_slots.as_mut() else {
                    return false;
                };
                let amount_u8 = u8::try_from(amount).unwrap_or(u8::MAX);
                pool.current = pool.current.saturating_add(amount_u8).min(pool.max);
                true
            }
            ResourceTargetKind::ClassPool => {
                let Some(id) = target.resource else {
                    return false;
                };
                let Some(pool) = self.class_pools.get_mut(&id) else {
                    return false;
                };
                pool.current = pool.current.saturating_add(amount).min(pool.max);
                true
            }
            ResourceTargetKind::FeatureUse => {
                let Some(id) = target.feature_id.as_ref() else {
                    return false;
                };
                let Some(pool) = self.feature_uses.get_mut(id) else {
                    return false;
                };
                pool.current = pool.current.saturating_add(amount).min(pool.max);
                true
            }
            ResourceTargetKind::ItemCharge => {
                let Some(id) = target.stack_id else {
                    return false;
                };
                let Some(pool) = self.item_charges.get_mut(&id) else {
                    return false;
                };
                pool.current = pool.current.saturating_add(amount).min(pool.max);
                true
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Group 5: Monster Profile
// ---------------------------------------------------------------------------

/// Monster-specific combat rating and flat initiative bonus (omitted on player characters).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct MonsterProfile {
    /// Challenge Rating stored in exact eighths (`0` = CR 0, `1` = 1/8, `2` = 1/4, `4` = 1/2,
    /// `8` = CR 1, ..., `240` = CR 30).
    pub challenge_rating_eighths: u16,
    /// Monster Initiative modifier from its stat block.
    pub initiative_bonus: i8,
}

#[cfg(test)]
mod tests {
    use super::super::assert_json_round_trip;
    use super::*;

    #[test]
    fn damage_math_applies_threshold_affinities_temp_hp_and_clamping() {
        let mut hp = Health {
            current: 20,
            max: 20,
            temporary: 5,
        };
        let affinities = DamageAffinities {
            resistances: BTreeSet::from([DamageType::Fire]),
            immunities: BTreeSet::from([DamageType::Poison]),
            vulnerabilities: BTreeSet::from([DamageType::Cold]),
        };

        // Immunity -> 0 damage.
        let out = hp.apply_damage(12, DamageType::Poison, 0, Some(&affinities), None);
        assert_eq!(out.effective_damage, 0);
        assert_eq!(hp.current, 20);

        // Below damage threshold -> 0 damage.
        let out = hp.apply_damage(9, DamageType::Slashing, 10, Some(&affinities), None);
        assert_eq!(out.effective_damage, 0);
        assert_eq!(hp.current, 20);

        // Resistance halves rounded down (9 -> 4), absorbed by temp HP (5 -> 1).
        let out = hp.apply_damage(9, DamageType::Fire, 0, Some(&affinities), None);
        assert_eq!(out.effective_damage, 4);
        assert_eq!(out.absorbed_by_temp, 4);
        assert_eq!(out.hp_lost, 0);
        assert_eq!(hp.temporary, 1);

        // Vulnerability doubles (6 -> 12), 1 absorbed by temp HP, 11 from current (20 -> 9, bloodied).
        let out = hp.apply_damage(6, DamageType::Cold, 0, Some(&affinities), None);
        assert_eq!(out.effective_damage, 12);
        assert_eq!(out.absorbed_by_temp, 1);
        assert_eq!(out.hp_lost, 11);
        assert_eq!(hp.current, 9);
        assert!(hp.is_bloodied());

        // Non-stacking temp HP takes max.
        hp.apply_temp_hp(7);
        hp.apply_temp_hp(3);
        assert_eq!(hp.temporary, 7);

        // Healing clamps to max.
        assert_eq!(hp.apply_heal(50), 11);
        assert_eq!(hp.current, 20);
        assert!(!hp.is_bloodied());
    }

    #[test]
    fn multiclass_sheet_with_overrides_dice_buffs_charges_and_innate_spells_round_trips() {
        let abilities = AbilityScores {
            strength: 16,
            dexterity: 14,
            constitution: 16,
            intelligence: 10,
            wisdom: 13,
            charisma: 8,
        };
        let mut effects = ActiveEffects::default();
        // Belt setting STR minimum to 25 + Bless +1d4 + Defense +1 AC.
        effects.upsert(ActiveEffect {
            id: "giant_belt".into(),
            name: "Belt of Fire Giant Strength".into(),
            source: None,
            duration_rounds: None,
            modifiers: vec![StatModifier {
                kind: StatModifierKind::AbilityScoreMin,
                value: 25,
                dice: None,
                dice_negative: false,
                roll_scope: None,
                ability: Some(Ability::Strength),
                skill: None,
                damage_type: None,
            }],
        });
        effects.upsert(ActiveEffect {
            id: "bless".into(),
            name: "Bless".into(),
            source: Some(RulesetId::new("bless").expect("valid id")),
            duration_rounds: Some(10),
            modifiers: vec![StatModifier {
                kind: StatModifierKind::RollBonus,
                value: 0,
                dice: Some(Dice { count: 1, sides: 4 }),
                dice_negative: false,
                roll_scope: Some(RollScope::SavingThrows),
                ability: None,
                skill: None,
                damage_type: None,
            }],
        });
        assert_eq!(
            abilities.effective_score(Ability::Strength, Some(&effects)),
            25
        );
        assert_eq!(abilities.modifier(Ability::Strength, Some(&effects)), 7);

        let mut classes = ClassLevels::single(ClassId::Fighter, 14, Some(SubclassId::Champion));
        assert!(classes.add_level(ClassId::Cleric, None));
        assert_eq!(classes.total_level(), 15);
        assert_eq!(classes.proficiency_bonus(), 5);
        assert_eq!(classes.hit_dice.len(), 2);
    }

    #[test]
    fn resource_pools_spell_dc_and_coin_purse_follow_5e_rules() {
        let abilities = AbilityScores {
            strength: 16,
            dexterity: 14,
            constitution: 16,
            intelligence: 10,
            wisdom: 13,
            charisma: 8,
        };
        let mut pools = ResourcePools::default();
        pools.spell_slots[0] = SlotPool { current: 2, max: 2 };
        pools.class_pools.insert(
            ResourceId::ActionSurge,
            ResourcePool {
                current: 1,
                max: 1,
                recharge: RechargeSchedule::ShortRest,
            },
        );
        pools.item_charges.insert(
            StackId(42),
            ResourcePool {
                current: 7,
                max: 7,
                recharge: RechargeSchedule::Dawn,
            },
        );
        let wand_target = ResourceTarget {
            kind: ResourceTargetKind::ItemCharge,
            spell_level: None,
            resource: None,
            feature_id: None,
            stack_id: Some(StackId(42)),
        };
        assert!(pools.spend(&wand_target, 2));
        assert_eq!(pools.item_charges[&StackId(42)].current, 5);
        assert!(pools.restore(&wand_target, 10));
        assert_eq!(pools.item_charges[&StackId(42)].current, 7);

        let spellbook = Spellbook {
            entries: vec![SpellEntry {
                spell_id: RulesetId::new("healing_word").expect("id"),
                spell_level: 1,
                source: SpellSourceKind::Class,
                source_class: Some(ClassId::Cleric),
                source_item: None,
                casting_ability: Some(Ability::Wisdom),
                fixed_dc: None,
                prep_state: SpellPrepState::Prepared,
                charge_cost: 0,
            }],
        };
        assert_eq!(
            Spellbook::spell_save_dc(&spellbook.entries[0], abilities, 5, None),
            Some(14)
        );

        let coins = CoinPurse {
            cp: 7,
            sp: 8,
            ep: 0,
            gp: 0,
            pp: 25,
        };
        assert_eq!(coins.total_copper(), 25_087);
    }

    #[test]
    fn all_23_property_components_round_trip_through_json() {
        assert_json_round_trip(&Health::full(132));
        assert_json_round_trip(&ArmorClass::new(24));
        assert_json_round_trip(&Size::Medium);
        assert_json_round_trip(&DamageAffinities::object_default());
        assert_json_round_trip(&ConditionImmunities::default());
        assert_json_round_trip(&CreatureType::Humanoid);
        assert_json_round_trip(&AbilityScores::default());
        assert_json_round_trip(&Speed::default());
        assert_json_round_trip(&Senses {
            darkvision: 60,
            ..Senses::default()
        });
        assert_json_round_trip(&Proficiencies::default());
        assert_json_round_trip(&Species {
            id: SpeciesId::Human,
        });
        assert_json_round_trip(&Background {
            id: BackgroundId::Soldier,
        });
        assert_json_round_trip(&ClassLevels::single(
            ClassId::Fighter,
            14,
            Some(SubclassId::Champion),
        ));
        assert_json_round_trip(&CharacterTraits::default());
        assert_json_round_trip(&Spellbook::default());
        assert_json_round_trip(&CoinPurse::default());
        assert_json_round_trip(&CharacterBio::default());
        assert_json_round_trip(&Conditions::default());
        assert_json_round_trip(&DeathSaves::default());
        assert_json_round_trip(&Concentration::default());
        assert_json_round_trip(&ActiveEffects::default());
        assert_json_round_trip(&ResourcePools::default());
        assert_json_round_trip(&MonsterProfile {
            challenge_rating_eighths: 2,
            initiative_bonus: 2,
        });
    }

    #[test]
    fn exhaustion_and_condition_immunities_follow_2024_rules() {
        let mut conds = Conditions::default();
        let immunities = ConditionImmunities {
            immune: BTreeSet::from([ConditionId::Charmed]),
        };
        let broke = conds.apply_changes(
            &[ConditionId::Charmed, ConditionId::Stunned],
            &[],
            2,
            &[HazardId::Burning],
            &[],
            Some(&immunities),
        );
        assert!(broke);
        assert!(!conds.active.contains(&ConditionId::Charmed));
        assert!(conds.active.contains(&ConditionId::Stunned));
        assert!(conds.active.contains(&ConditionId::Exhaustion));
        assert_eq!(conds.exhaustion_level, 2);
        assert_eq!(conds.d20_test_penalty(), -4);
        assert_eq!(conds.speed_penalty_ft(), 10);
        // 2024 5e: Stunned does NOT zero speed.
        assert!(!conds.zeros_speed());
        assert_eq!(Speed::walk_only(30).effective_walk(Some(&conds), None), 20);
    }
}
