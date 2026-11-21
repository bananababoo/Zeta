# AI Orchestration

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | AI DM request contents, Snapshot, inspect tool, output schema, dice resolution, output validation, prompt injection, and provider interface |
| **Last Updated** | `2026-11-21` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This document defines how the server builds AI DM requests, validates and applies AI DM responses, and communicates with AI providers. The AI DM makes every judgment, and the server checks only structure. The server therefore needs no code for any specific effect or entity type.

---

## 1. Request Contents & Snapshot

### What
For each Freeform Action Prompt, the server builds a **Snapshot** of every entity the acting character could plausibly perceive (line of sight plus hearing range), capped at a maximum count. Every entity in the Snapshot uses the same fixed field set: local integer identifier, name, grid position, tags, passable, and a 1-sentence description. Local integer identifiers (`0, 1, 2...`) are assigned per request.

Besides the system prompt, the player text, and the Snapshot, every request includes exactly 2 more parts, both in fixed fields. The first is a short summary of the acting character (name, level, class, current hit points and conditions, key abilities, and carried items). The second is the setting essentials (current Place name, time of day from the World Clock or Area Clock, and whether a Turn Bubble is active). Requests include no recent events, Lore, or history by default, and the AI DM fetches them through the inspect tool in Section 2.

### Why
A chain reaction can only reach what the character could perceive, so the Snapshot holds everything the AI DM needs and no secrets the character could not know. A bridge, a goblin, and a barrel use the same fields, so no entity type needs its own code, and short identifiers keep responses small. History is long and most actions do not need it, so the AI DM fetches it through the inspect tool only when an action does.

### Boundaries
Does not define the maximum count.

---

## 2. Inspect Tool

### What
Before returning its final resolution, the AI DM may call an inspect tool through native provider tool calling, passing entity identifiers, the current Place, or a Lore Entry identifier to receive full details such as history, recent witnessed events, long descriptions, and Lore. The number of inspect rounds per action is capped. When the AI DM inspects the history of a Place, it receives the events of the current era 1 by 1 and each older era only as its 1-line summary, and it inspects an older era to receive that era's events.

### Why
Requests stay small, and the AI DM can still fetch details when an action needs them. Models are trained on native tool calling and use it more reliably than a custom request format. The cap stops 1 action from holding up a scene. A Place can have thousands of years of events, and most scenes need only the recent ones.

### Boundaries
Does not define the exact round cap or the full list of inspectable details.

---

## 3. Output & Dice Resolution

### What
The AI DM returns an `AiResolution` containing `chk`, `fx`, and `fail` branches. The server does **not** pre-roll d20s before calling the AI DM. Bevy rolls the visible d20 on screen to execute the matching branch, and the AI DM sets only the target Difficulty Class and **never chooses a roll bonus**.

`WorldEffect` is a typed Rust enum. On the wire, each effect is a JSON object whose `op` field names the effect and whose other fields are named keys, so provider structured-output modes enforce the schema at generation time. Booleans are `true` or `false`, enumerated values are lowercase strings, tag changes use separate `add` and `remove` lists, and omitted optional fields mean no change. Every settling process, whether on a modified or a spawned entity, uses the same `settle` object.

### Why
The AI DM sets the Difficulty Class before the roll exists, so it cannot pick a Difficulty Class to steer a known result. With a typed enum, the compiler reports any `WorldEffect` variant the server does not handle. Provider structured-output modes enforce named keys, so malformed output is rare, and the Longevity Principle values that over the tokens positional tuples would save.

### Boundaries
Prototype-specific prompt wording is isolated in [`../prototypes/realism_prototype.md`](../prototypes/realism_prototype.md).

---

## 4. Output Validation & Retry

### What
Before applying a response, the server performs structural checks only: the output matches the schema with no unknown fields, every referenced identifier exists in that request's Snapshot, and every coordinate is inside the map. When a check fails, the server sends the exact validation error back to the AI DM and retries automatically, up to a cap. If the retries are exhausted, the server applies no effects from that response, releases the `PendingAiResolution` lock, and asks the player to rephrase.

### Why
The same checks apply to every effect, so validation needs no gameplay code. An unknown field usually means a misspelled key, and ignoring it would silently drop part of the AI DM's intent, so it fails the check and is retried. Models usually fix the error on the first retry, so the player rarely notices. The server applies nothing from a failed response, so a chain reaction never stops halfway.

### Boundaries
Does not judge whether an outcome is plausible; that judgment belongs to the AI DM.

---

## 5. Prompt Injection

### What
Player text is always passed to the AI DM in a clearly delimited, untrusted field. The system prompt states that this field is only an in-world action declaration and never contains instructions. The scenario test suite includes prompt-injection attempts.

### Why
A player can write text that manipulates the AI DM into a response that is still structurally valid, and validation cannot catch it. The request itself therefore keeps player text apart from instructions.

### Boundaries
Does not define content moderation, which is defined in [`technical_architecture.md`](technical_architecture.md) Section 3.

---

## 6. Provider Interface

### What
All AI calls go through a provider-agnostic Rust trait. A recording layer saves every request and response so tests can replay them without calling or paying a provider. The prototype uses the Gemini free tier. The model name and the key come from server configuration, never from code.

### Why
Developers can switch providers or models without changing game code. Recorded scenarios replay with the same results every time and cost nothing. When a provider updates a model, developers rerun the recorded scenarios to see what changed. The free tier costs nothing during development, and model names change often, so configuration holds the model name.

### Boundaries
Does not choose the provider for launch; that choice waits for cost and quality data from playtests.

---

## Schemas

### AI Resolution Schema
```rust
use serde::{Deserialize, Serialize};

/// Local integer identifier assigned per request in the Snapshot.
pub type LocalId = u32;

#[derive(Serialize, Deserialize, Debug, Clone)]
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

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub dc: u8,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Volume {
    Whisper,
    Normal,
    Shout,
}

/// Final state an entity settles into after an unstable process ends.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct Settle {
    /// In-game minutes until the entity settles
    pub minutes: u32,
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
    pub passable: Option<bool>,
    pub visual: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
pub enum WorldEffect {
    /// {"op": "narrate", "text": "The rope snaps."}
    Narrate { text: String },
    /// {"op": "speak", "id": 3, "text": "Halt!", "vol": "shout"}
    Speak { id: LocalId, text: String, vol: Volume },
    /// {"op": "move", "id": 3, "x": 12, "y": 7, "style": "stumbles"}
    Move { id: LocalId, x: u32, y: u32, style: String },
    /// {"op": "modify", "id": 5, "add": ["burning"], "remove": ["dry"], "passable": false, "visual": "a flaming cart", "settle": {...}}
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
    /// {"op": "spawn", "name": "Smoke", "x": 12, "y": 8, "passable": true, "tags": ["obscuring"], "visual": "thick grey smoke", "settle": {...}}
    Spawn {
        name: String,
        x: u32,
        y: u32,
        passable: bool,
        tags: Vec<String>,
        visual: String,
        settle: Option<Settle>,
    },
    /// {"op": "destroy", "id": 5, "reason": "burned to ash"}
    Destroy { id: LocalId, reason: String },
    /// {"op": "history", "ids": [3, 5], "summary": "The guard watched the cart burn.", "preserved": true}
    History { ids: Vec<LocalId>, summary: String, preserved: bool },
    /// {"op": "bubble", "active": true, "ids": [0, 3]}
    Bubble { active: bool, ids: Vec<LocalId> },
}
```

---

## Open Questions
1. **Snapshot Maximum Count:** What is the maximum number of entities in a Snapshot?
