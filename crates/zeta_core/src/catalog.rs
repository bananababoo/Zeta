//! The shared item catalog: standard equipment, weapons, armor, and tools from System Reference
//! Document 5.2.1, compiled into both the server and the client.
//!
//! Rules: `docs/design/game_design.md` Section 5. The data lives in `data/srd_catalog.json`, generated
//! by `tools/import_srd_catalog.ps1` and embedded at compile time, so this module performs no input or
//! output. Attribution for the data: `data/ATTRIBUTION.md`.
//!
//! Catalog types carry no `skip_serializing_if`, so they also round-trip through the binary network
//! format; optional fields missing from the JSON file read as empty through `#[serde(default)]`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The embedded SRD 5.2.1 catalog data.
const SRD_CATALOG_JSON: &str = include_str!("../data/srd_catalog.json");

/// The longest identifier a catalog entry may have.
const MAX_CATALOG_ID_LENGTH: usize = 64;

/// The stable identifier of a catalog entry, such as `painters_supplies`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CatalogId(String);

/// Text that cannot be a catalog identifier.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error(
    "\"{0}\" is not a catalog id; ids are 1 to 64 characters of lowercase a-z, digits 0-9, and _"
)]
pub struct InvalidCatalogId(pub String);

impl CatalogId {
    /// Wraps an identifier.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidCatalogId`] unless `id` is 1 to 64 characters of lowercase `a-z`, `0-9`, and `_`.
    pub fn new(id: impl Into<String>) -> Result<Self, InvalidCatalogId> {
        let id = id.into();
        let valid_characters = id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if valid_characters && !id.is_empty() && id.len() <= MAX_CATALOG_ID_LENGTH {
            Ok(Self(id))
        } else {
            Err(InvalidCatalogId(id))
        }
    }

    /// The identifier text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CatalogId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for CatalogId {
    type Error = InvalidCatalogId;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        Self::new(id)
    }
}

impl From<CatalogId> for String {
    fn from(id: CatalogId) -> Self {
        id.0
    }
}

/// 1 catalog entry and its passive traits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogItem {
    /// Stable identifier.
    #[serde(rename = "id")]
    pub id: CatalogId,
    /// Display name from the SRD.
    #[serde(rename = "name")]
    pub name: String,
    /// Equipment table the entry belongs to.
    #[serde(rename = "category")]
    pub category: ItemCategory,
    /// Purchase price; absent for entries with no listed price, such as item groups.
    #[serde(rename = "cost", default)]
    pub cost: Option<Cost>,
    /// Weight in thousandths of a pound, so ammunition weights stay whole numbers; 0 is negligible.
    #[serde(rename = "weight_thousandths_of_pound")]
    pub weight_thousandths_of_pound: u32,
    /// Whether using the item uses it up, such as Rations, Oil, or a poison; these offer `Consume`.
    #[serde(rename = "consumable", default)]
    pub consumable: bool,
    /// Weapon traits, for weapons.
    #[serde(rename = "weapon", default)]
    pub weapon: Option<WeaponData>,
    /// Armor traits, for armor and shields.
    #[serde(rename = "armor", default)]
    pub armor: Option<ArmorData>,
    /// Tool traits, for tools.
    #[serde(rename = "tool", default)]
    pub tool: Option<ToolData>,
    /// Checks attached to the Freeform Action Prompt when the item is used with `Utilize`.
    #[serde(rename = "utilize", default)]
    pub utilize: Vec<UtilizeCheck>,
    /// Entries the SRD says this tool makes; an item group means any of its members. Source data
    /// only: nothing uses or shows it until crafting is designed.
    #[serde(rename = "crafts", default)]
    pub crafts: Vec<CatalogId>,
    /// What a pack or bundle holds.
    #[serde(rename = "contents", default)]
    pub contents: Vec<PackEntry>,
    /// The entries an item group stands for, such as the 3 kinds of Holy Symbol.
    #[serde(rename = "group_members", default)]
    pub group_members: Vec<CatalogId>,
}

impl CatalogItem {
    /// Whether this entry names a set of items rather than 1 item a character can hold.
    pub fn is_group(&self) -> bool {
        self.category == ItemCategory::ItemGroup
    }
}

/// The equipment table a catalog entry belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemCategory {
    /// A melee or ranged weapon.
    Weapon,
    /// Armor or a shield.
    Armor,
    /// Ammunition, single or bundled.
    Ammunition,
    /// Artisan's tools, gaming sets, musical instruments, and other tools.
    Tool,
    /// An arcane focus, druidic focus, or holy symbol.
    SpellcastingFocus,
    /// Adventuring gear, including packs.
    AdventuringGear,
    /// Food and drink.
    FoodAndDrink,
    /// Tack and harness for mounts.
    TackAndHarness,
    /// A named set of items, such as Holy Symbol or Artisan's Tools.
    ItemGroup,
}

/// A purchase price: `copper_pieces` buys `per_quantity` units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    /// Price in copper pieces.
    #[serde(rename = "copper_pieces")]
    pub copper_pieces: u32,
    /// Units bought for that price; above 1 only where a single unit costs a fraction of a copper piece.
    #[serde(rename = "per_quantity", default = "one")]
    pub per_quantity: u32,
}

/// The default `per_quantity` of a [`Cost`].
const fn one() -> u32 {
    1
}

/// Weapon traits from the SRD weapon table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaponData {
    /// Simple or martial.
    #[serde(rename = "category")]
    pub category: WeaponCategory,
    /// Melee or ranged.
    #[serde(rename = "kind")]
    pub kind: WeaponKind,
    /// Damage dice on a hit.
    #[serde(rename = "damage_dice")]
    pub damage_dice: Dice,
    /// Damage type on a hit.
    #[serde(rename = "damage_type")]
    pub damage_type: DamageType,
    /// Damage dice when wielded with 2 hands, for versatile weapons.
    #[serde(rename = "versatile_dice", default)]
    pub versatile_dice: Option<Dice>,
    /// Weapon properties.
    #[serde(rename = "properties", default)]
    pub properties: Vec<WeaponProperty>,
    /// Normal and long range, for ranged and thrown weapons.
    #[serde(rename = "range", default)]
    pub range: Option<WeaponRange>,
    /// Weapon mastery property.
    #[serde(rename = "mastery")]
    pub mastery: WeaponMastery,
    /// The ammunition the weapon fires, for weapons with the ammunition property.
    #[serde(rename = "ammunition", default)]
    pub ammunition: Option<CatalogId>,
}

/// Simple or martial weapon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeaponCategory {
    /// Simple weapon.
    Simple,
    /// Martial weapon.
    Martial,
}

/// Melee or ranged weapon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeaponKind {
    /// Melee weapon.
    Melee,
    /// Ranged weapon.
    Ranged,
}

/// A number of dice of 1 size. A flat amount N, such as the Blowgun's 1 damage, is N dice of 1 side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dice {
    /// How many dice.
    #[serde(rename = "count")]
    pub count: u8,
    /// Sides per die.
    #[serde(rename = "sides")]
    pub sides: u8,
}

/// The 13 damage types in System Reference Document 5.2.1.
///
/// The first 3 variants keep their original order for wire compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DamageType {
    /// Bludgeoning damage.
    Bludgeoning,
    /// Piercing damage.
    Piercing,
    /// Slashing damage.
    Slashing,
    /// Acid damage.
    Acid,
    /// Cold damage.
    Cold,
    /// Fire damage.
    Fire,
    /// Force damage.
    Force,
    /// Lightning damage.
    Lightning,
    /// Necrotic damage.
    Necrotic,
    /// Poison damage.
    Poison,
    /// Psychic damage.
    Psychic,
    /// Radiant damage.
    Radiant,
    /// Thunder damage.
    Thunder,
}

impl DamageType {
    /// Every System Reference Document 5.2.1 damage type in canonical order.
    pub const ALL: [Self; 13] = [
        Self::Bludgeoning,
        Self::Piercing,
        Self::Slashing,
        Self::Acid,
        Self::Cold,
        Self::Fire,
        Self::Force,
        Self::Lightning,
        Self::Necrotic,
        Self::Poison,
        Self::Psychic,
        Self::Radiant,
        Self::Thunder,
    ];

    /// Canonical lowercase `snake_case` ruleset identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bludgeoning => "bludgeoning",
            Self::Piercing => "piercing",
            Self::Slashing => "slashing",
            Self::Acid => "acid",
            Self::Cold => "cold",
            Self::Fire => "fire",
            Self::Force => "force",
            Self::Lightning => "lightning",
            Self::Necrotic => "necrotic",
            Self::Poison => "poison",
            Self::Psychic => "psychic",
            Self::Radiant => "radiant",
            Self::Thunder => "thunder",
        }
    }
}

/// SRD weapon properties.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeaponProperty {
    /// Needs ammunition to make a ranged attack.
    Ammunition,
    /// May use Dexterity instead of Strength.
    Finesse,
    /// Hard to wield for small or weak wielders.
    Heavy,
    /// Usable for 2-weapon fighting.
    Light,
    /// 1 shot per action, bonus action, or reaction.
    Loading,
    /// Adds 5 feet to reach.
    Reach,
    /// Can be thrown for a ranged attack.
    Thrown,
    /// Needs 2 hands.
    TwoHanded,
    /// Can be used with 1 or 2 hands.
    Versatile,
}

/// Normal and long range of a ranged or thrown weapon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaponRange {
    /// Normal range in feet.
    #[serde(rename = "normal_feet")]
    pub normal_feet: u16,
    /// Long range in feet.
    #[serde(rename = "long_feet")]
    pub long_feet: u16,
}

/// SRD weapon mastery properties.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeaponMastery {
    /// Cleave mastery.
    Cleave,
    /// Graze mastery.
    Graze,
    /// Nick mastery.
    Nick,
    /// Push mastery.
    Push,
    /// Sap mastery.
    Sap,
    /// Slow mastery.
    Slow,
    /// Topple mastery.
    Topple,
    /// Vex mastery.
    Vex,
}

/// Armor traits from the SRD armor table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmorData {
    /// Light, medium, heavy, or shield.
    #[serde(rename = "category")]
    pub category: ArmorCategory,
    /// Base Armor Class, or the bonus for a shield.
    #[serde(rename = "armor_class")]
    pub armor_class: u8,
    /// Strength score needed to avoid the speed penalty, for heavy armor that has one.
    #[serde(rename = "strength_requirement", default)]
    pub strength_requirement: Option<u8>,
    /// Whether the armor gives Disadvantage on Dexterity (Stealth) checks.
    #[serde(rename = "stealth_disadvantage", default)]
    pub stealth_disadvantage: bool,
}

/// SRD armor categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmorCategory {
    /// Light armor.
    Light,
    /// Medium armor.
    Medium,
    /// Heavy armor.
    Heavy,
    /// Shield.
    Shield,
}

/// Tool traits from the SRD tool cards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolData {
    /// Which tool table the tool belongs to.
    #[serde(rename = "kind")]
    pub kind: ToolKind,
    /// Ability used for checks with the tool.
    #[serde(rename = "ability")]
    pub ability: Ability,
}

/// SRD tool tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Artisan's tools.
    Artisans,
    /// Gaming set.
    GamingSet,
    /// Musical instrument.
    MusicalInstrument,
    /// Any other tool, such as Thieves' Tools.
    Other,
}

/// The 6 abilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ability {
    /// Strength.
    Strength,
    /// Dexterity.
    Dexterity,
    /// Constitution.
    Constitution,
    /// Intelligence.
    Intelligence,
    /// Wisdom.
    Wisdom,
    /// Charisma.
    Charisma,
}

impl Ability {
    /// All 6 abilities in standard character-sheet order.
    pub const ALL: [Self; 6] = [
        Self::Strength,
        Self::Dexterity,
        Self::Constitution,
        Self::Intelligence,
        Self::Wisdom,
        Self::Charisma,
    ];

    /// Canonical lowercase identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strength => "strength",
            Self::Dexterity => "dexterity",
            Self::Constitution => "constitution",
            Self::Intelligence => "intelligence",
            Self::Wisdom => "wisdom",
            Self::Charisma => "charisma",
        }
    }
}

/// 1 check an item offers through `Utilize`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UtilizeCheck {
    /// What the check does, such as "Pick a lock".
    #[serde(rename = "description")]
    pub description: String,
    /// Difficulty Class of the check.
    #[serde(rename = "dc")]
    pub dc: u8,
    /// Ability the check uses, when the SRD names one for this check; tool checks use
    /// [`ToolData::ability`] instead.
    #[serde(rename = "ability", default)]
    pub ability: Option<Ability>,
    /// Skill the check uses, when the SRD names one.
    #[serde(rename = "skill", default)]
    pub skill: Option<Skill>,
}

/// The 18 skills.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Skill {
    /// Acrobatics (Dexterity).
    Acrobatics,
    /// Animal Handling (Wisdom).
    AnimalHandling,
    /// Arcana (Intelligence).
    Arcana,
    /// Athletics (Strength).
    Athletics,
    /// Deception (Charisma).
    Deception,
    /// History (Intelligence).
    History,
    /// Insight (Wisdom).
    Insight,
    /// Intimidation (Charisma).
    Intimidation,
    /// Investigation (Intelligence).
    Investigation,
    /// Medicine (Wisdom).
    Medicine,
    /// Nature (Intelligence).
    Nature,
    /// Perception (Wisdom).
    Perception,
    /// Performance (Charisma).
    Performance,
    /// Persuasion (Charisma).
    Persuasion,
    /// Religion (Intelligence).
    Religion,
    /// Sleight of Hand (Dexterity).
    SleightOfHand,
    /// Stealth (Dexterity).
    Stealth,
    /// Survival (Wisdom).
    Survival,
}

impl Skill {
    /// All 18 skills in alphabetical order.
    pub const ALL: [Self; 18] = [
        Self::Acrobatics,
        Self::AnimalHandling,
        Self::Arcana,
        Self::Athletics,
        Self::Deception,
        Self::History,
        Self::Insight,
        Self::Intimidation,
        Self::Investigation,
        Self::Medicine,
        Self::Nature,
        Self::Perception,
        Self::Performance,
        Self::Persuasion,
        Self::Religion,
        Self::SleightOfHand,
        Self::Stealth,
        Self::Survival,
    ];

    /// Canonical lowercase `snake_case` identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Acrobatics => "acrobatics",
            Self::AnimalHandling => "animal_handling",
            Self::Arcana => "arcana",
            Self::Athletics => "athletics",
            Self::Deception => "deception",
            Self::History => "history",
            Self::Insight => "insight",
            Self::Intimidation => "intimidation",
            Self::Investigation => "investigation",
            Self::Medicine => "medicine",
            Self::Nature => "nature",
            Self::Perception => "perception",
            Self::Performance => "performance",
            Self::Persuasion => "persuasion",
            Self::Religion => "religion",
            Self::SleightOfHand => "sleight_of_hand",
            Self::Stealth => "stealth",
            Self::Survival => "survival",
        }
    }

    /// Default governing ability for this skill in System Reference Document 5.2.1.
    pub const fn ability(self) -> Ability {
        match self {
            Self::Athletics => Ability::Strength,
            Self::Acrobatics | Self::SleightOfHand | Self::Stealth => Ability::Dexterity,
            Self::Arcana | Self::History | Self::Investigation | Self::Nature | Self::Religion => {
                Ability::Intelligence
            }
            Self::AnimalHandling
            | Self::Insight
            | Self::Medicine
            | Self::Perception
            | Self::Survival => Ability::Wisdom,
            Self::Deception | Self::Intimidation | Self::Performance | Self::Persuasion => {
                Ability::Charisma
            }
        }
    }
}

/// 1 line of a pack or bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackEntry {
    /// The entry held.
    #[serde(rename = "item")]
    pub item: CatalogId,
    /// How many.
    #[serde(rename = "quantity")]
    pub quantity: u32,
}

/// Why catalog data could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CatalogError {
    /// The text is not valid catalog JSON.
    #[error("catalog JSON does not parse: {0}")]
    Parse(String),
    /// 2 entries share an identifier; identifiers must be unique.
    #[error("catalog id {0} appears more than once; ids must be unique")]
    DuplicateId(CatalogId),
    /// An entry refers to an identifier that has no entry.
    #[error("catalog entry {from} refers to {to}, which is not in the catalog")]
    UnknownReference {
        /// The entry holding the reference.
        from: CatalogId,
        /// The identifier that has no entry.
        to: CatalogId,
    },
}

/// The file layout of `srd_catalog.json`.
#[derive(Deserialize)]
struct CatalogFile {
    #[serde(rename = "items")]
    items: Vec<CatalogItem>,
}

/// Every catalog entry, keyed by identifier.
#[derive(Clone, Debug, PartialEq)]
pub struct Catalog {
    items: BTreeMap<CatalogId, CatalogItem>,
}

impl Catalog {
    /// The SRD 5.2.1 catalog embedded in the build, parsed on first use.
    ///
    /// # Panics
    ///
    /// Never in practice: the embedded file is generated, and the tests prove it parses and resolves.
    pub fn srd() -> &'static Self {
        static SRD: OnceLock<Catalog> = OnceLock::new();
        SRD.get_or_init(|| {
            Self::from_json(SRD_CATALOG_JSON)
                .expect("the embedded SRD catalog parses and resolves; the tests check it")
        })
    }

    /// Parses catalog JSON and checks that identifiers are unique and every reference resolves.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] when the JSON does not parse, an identifier repeats, or a reference
    /// names an identifier with no entry.
    pub fn from_json(text: &str) -> Result<Self, CatalogError> {
        let file: CatalogFile =
            serde_json::from_str(text).map_err(|error| CatalogError::Parse(error.to_string()))?;
        let mut items = BTreeMap::new();
        for item in file.items {
            if items.contains_key(&item.id) {
                return Err(CatalogError::DuplicateId(item.id));
            }
            items.insert(item.id.clone(), item);
        }
        let catalog = Self { items };
        catalog.check_references()?;
        Ok(catalog)
    }

    fn check_references(&self) -> Result<(), CatalogError> {
        for item in self.items.values() {
            let references = item
                .crafts
                .iter()
                .chain(item.group_members.iter())
                .chain(item.contents.iter().map(|entry| &entry.item))
                .chain(
                    item.weapon
                        .iter()
                        .filter_map(|weapon| weapon.ammunition.as_ref()),
                );
            for reference in references {
                if !self.contains(reference) {
                    return Err(CatalogError::UnknownReference {
                        from: item.id.clone(),
                        to: reference.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// The entry with this identifier.
    pub fn get(&self, id: &CatalogId) -> Option<&CatalogItem> {
        self.items.get(id)
    }

    /// Whether an entry has this identifier.
    pub fn contains(&self, id: &CatalogId) -> bool {
        self.items.contains_key(id)
    }

    /// Every entry, sorted by identifier.
    pub fn items(&self) -> impl Iterator<Item = &CatalogItem> {
        self.items.values()
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the catalog has no entries.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Entries whose name or identifier contains every word of `query`, ignoring case.
    ///
    /// An entry whose name or identifier equals the whole query comes first; the rest follow by
    /// identifier. An empty query matches nothing.
    pub fn search(&self, query: &str) -> Vec<&CatalogItem> {
        let query = query.trim().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        if words.is_empty() {
            return Vec::new();
        }
        let mut exact = Vec::new();
        let mut partial = Vec::new();
        for item in self.items.values() {
            let name = item.name.to_lowercase();
            let id = item.id.as_str();
            if name == query || id == query {
                exact.push(item);
            } else if words
                .iter()
                .all(|word| name.contains(word) || id.contains(word))
            {
                partial.push(item);
            }
        }
        exact.extend(partial);
        exact
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> CatalogId {
        CatalogId::new(text).expect("test ids are valid")
    }

    #[test]
    fn embedded_srd_catalog_parses_with_unique_ids_and_resolved_references() {
        let catalog = Catalog::from_json(SRD_CATALOG_JSON).expect("embedded catalog loads");
        assert!(catalog.len() > 150, "catalog has {} entries", catalog.len());
        assert_eq!(Catalog::srd().len(), catalog.len());
    }

    #[test]
    fn every_id_in_the_embedded_file_is_unique() {
        let file: CatalogFile = serde_json::from_str(SRD_CATALOG_JSON).expect("file parses");
        let mut seen = std::collections::BTreeSet::new();
        for item in &file.items {
            assert!(seen.insert(item.id.clone()), "{} repeats", item.id);
        }
    }

    #[test]
    fn every_crafts_reference_resolves() {
        let catalog = Catalog::srd();
        for item in catalog.items() {
            for crafted in &item.crafts {
                assert!(
                    catalog.contains(crafted),
                    "{} crafts unknown {crafted}",
                    item.id
                );
            }
        }
    }

    #[test]
    fn painters_supplies_matches_the_srd_card() {
        let item = Catalog::srd()
            .get(&id("painters_supplies"))
            .expect("painters_supplies is in the catalog");
        assert_eq!(item.name, "Painter's Supplies");
        assert_eq!(item.category, ItemCategory::Tool);
        assert_eq!(
            item.tool,
            Some(ToolData {
                kind: ToolKind::Artisans,
                ability: Ability::Wisdom
            })
        );
        assert_eq!(
            item.cost,
            Some(Cost {
                copper_pieces: 1000,
                per_quantity: 1
            })
        );
        assert_eq!(item.weight_thousandths_of_pound, 5000);
        assert_eq!(item.utilize.len(), 1);
        assert_eq!(item.utilize[0].dc, 10);
        assert_eq!(item.crafts, vec![id("druidic_focus"), id("holy_symbol")]);
    }

    #[test]
    fn non_srd_player_handbook_items_are_absent() {
        // Psychic Blade is a 2024 Player's Handbook weapon without the srd52 flag.
        assert!(!Catalog::srd().contains(&id("psychic_blade")));
    }

    #[test]
    fn magic_items_mounts_and_vehicles_are_not_imported_yet() {
        let catalog = Catalog::srd();
        for name in [
            "spell_scroll",
            "potion_of_healing",
            "riding_horse",
            "rowboat",
            "stabling_per_day",
        ] {
            assert!(!catalog.contains(&id(name)), "{name} is out of scope");
        }
    }

    #[test]
    fn longsword_has_versatile_slashing_damage_and_sap_mastery() {
        let weapon = Catalog::srd()
            .get(&id("longsword"))
            .and_then(|item| item.weapon.clone())
            .expect("longsword is a weapon");
        assert_eq!(weapon.category, WeaponCategory::Martial);
        assert_eq!(weapon.kind, WeaponKind::Melee);
        assert_eq!(weapon.damage_dice, Dice { count: 1, sides: 8 });
        assert_eq!(
            weapon.versatile_dice,
            Some(Dice {
                count: 1,
                sides: 10
            })
        );
        assert_eq!(weapon.damage_type, DamageType::Slashing);
        assert_eq!(weapon.properties, vec![WeaponProperty::Versatile]);
        assert_eq!(weapon.mastery, WeaponMastery::Sap);
    }

    #[test]
    fn shortbow_fires_arrows_from_the_catalog() {
        let catalog = Catalog::srd();
        let weapon = catalog
            .get(&id("shortbow"))
            .and_then(|item| item.weapon.clone())
            .expect("shortbow is a weapon");
        assert_eq!(weapon.ammunition, Some(id("arrow")));
        assert_eq!(
            weapon.range,
            Some(WeaponRange {
                normal_feet: 80,
                long_feet: 320
            })
        );
    }

    #[test]
    fn plate_armor_needs_strength_15_and_hinders_stealth() {
        let armor = Catalog::srd()
            .get(&id("plate_armor"))
            .and_then(|item| item.armor)
            .expect("plate_armor is armor");
        assert_eq!(armor.category, ArmorCategory::Heavy);
        assert_eq!(armor.armor_class, 18);
        assert_eq!(armor.strength_requirement, Some(15));
        assert!(armor.stealth_disadvantage);
    }

    #[test]
    fn rope_offers_a_short_dexterity_sleight_of_hand_check() {
        let item = Catalog::srd()
            .get(&id("rope"))
            .expect("rope is in the catalog");
        assert_eq!(
            item.utilize,
            vec![UtilizeCheck {
                description: "Tie a knot with Rope".to_owned(),
                dc: 10,
                ability: Some(Ability::Dexterity),
                skill: Some(Skill::SleightOfHand),
            }]
        );
    }

    #[test]
    fn rations_are_consumable_and_rope_is_not() {
        let catalog = Catalog::srd();
        assert!(
            catalog
                .get(&id("rations"))
                .is_some_and(|item| item.consumable)
        );
        assert!(
            catalog
                .get(&id("rope"))
                .is_some_and(|item| !item.consumable)
        );
    }

    #[test]
    fn sling_bullet_costs_1_copper_per_5() {
        let item = Catalog::srd()
            .get(&id("sling_bullet"))
            .expect("sling_bullet is in the catalog");
        assert_eq!(
            item.cost,
            Some(Cost {
                copper_pieces: 1,
                per_quantity: 5
            })
        );
    }

    #[test]
    fn explorers_pack_lists_its_contents() {
        let item = Catalog::srd()
            .get(&id("explorers_pack"))
            .expect("explorers_pack is in the catalog");
        assert!(item.contents.contains(&PackEntry {
            item: id("torch"),
            quantity: 10
        }));
    }

    #[test]
    fn search_puts_exact_matches_first_and_ignores_case() {
        let catalog = Catalog::srd();
        let results = catalog.search("Rope");
        assert_eq!(results.first().map(|item| item.id.as_str()), Some("rope"));
        let thieves: Vec<&str> = catalog
            .search("THIEVES tools")
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(thieves, vec!["thieves_tools"]);
        assert_eq!(catalog.search("   ").len(), 0);
    }

    #[test]
    fn catalog_id_rejects_uppercase_spaces_and_empty_text() {
        assert!(CatalogId::new("Rope").is_err());
        assert!(CatalogId::new("iron spike").is_err());
        assert!(CatalogId::new("").is_err());
        assert!(CatalogId::new("a".repeat(65)).is_err());
        assert_eq!(id("arrows_20").as_str(), "arrows_20");
    }

    #[test]
    fn from_json_rejects_duplicate_ids_and_unknown_references() {
        let duplicate = r#"{"items": [
            {"id": "rope", "name": "Rope", "category": "adventuring_gear", "weight_thousandths_of_pound": 5000},
            {"id": "rope", "name": "Rope", "category": "adventuring_gear", "weight_thousandths_of_pound": 5000}
        ]}"#;
        assert_eq!(
            Catalog::from_json(duplicate),
            Err(CatalogError::DuplicateId(id("rope")))
        );
        let unknown = r#"{"items": [
            {"id": "kit", "name": "Kit", "category": "tool", "weight_thousandths_of_pound": 0, "crafts": ["missing"]}
        ]}"#;
        assert_eq!(
            Catalog::from_json(unknown),
            Err(CatalogError::UnknownReference {
                from: id("kit"),
                to: id("missing")
            })
        );
    }

    #[test]
    fn catalog_id_round_trips_as_a_plain_json_string() {
        let text = serde_json::to_string(&id("rope")).expect("serializes");
        assert_eq!(text, "\"rope\"");
        assert!(serde_json::from_str::<CatalogId>("\"Bad Id\"").is_err());
    }
}
