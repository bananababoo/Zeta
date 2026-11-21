# Game Design

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | Core gameplay vision, audience, world state, playstyles, combat, and stakes |
| **Last Updated** | `2026-11-16` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This document defines what the game is: a large multiplayer classic fantasy game for personal computers, inspired by D&D, sold on Steam, and built around players cooperating with each other. Players change a shared, persistent world, and the game runs on any decent computer. How time passes in the world is defined in [`time_model.md`](time_model.md).

---

## 1. Vision & Narrative

### What
The game uses a top-down VTT view on a fine grid, similar to MapTool or Roll20. An AI DM tells the story and referees: it controls enemies, judges whether actions are realistic, and answers metagaming with narrative consequences, with human review for serious cases. Foundational world rules and Lore, such as religion and gods, are fixed, and players create all new history. The AI DM does not try to detect external voice chat; it acts only on what each character witnessed or learned in-world, as recorded in the Event Log, and a character acting on knowledge it never gained in-world faces narrative consequences such as suspicion from NPCs.

Player storylines are non-linear, and the stories of different parties merge when their goals or travels cross. The game is easy to start for a broad audience, and its stakes and lasting history reward committed players who come back every week.

### Why
A pre-written plot breaks in a shared world as soon as 1 party defeats its villain early. An AI DM that reacts to a persistent world lets hundreds of parties change the same world at once, and players succeed or fail by their own decisions. An easy start brings in a broad audience, and the weekly cadence rewards showing up consistently over playing long hours, which keeps the committed players the shared world depends on.

### Boundaries
The AI DM never authors a railroaded plot, does not replace human roleplay, and cannot alter Lore.

---

## 2. World State & History

### What
The world map is pre-generated and fixed, and procedural encounters spawn on top of it. All world state and every change players make are stored in a database, never in an AI context window. The states of things in the world are free-form tags, not a fixed list, so anything plausible can happen, such as exploding or painting a bridge.

Any change lasting longer than about 5 in-game minutes is recorded in the history of the **Place** (location) and of every directly affected **Object**. History decays over time unless it is written down in-world, such as in books, scrolls, or inscriptions, or physically preserved, such as in ruins, monuments, or graves. Outside Turn Bubbles, an unstable state such as fire resolves through **Settling**: every nearby affected entity gets a `settle` timer on the World Clock at the moment the state starts. Timer behavior inside Bubble Areas and in unobserved areas is defined in [`time_model.md`](time_model.md) Sections 5 and 6.

### Why
An AI forgets anything outside its context window, so the database holds the world. A fixed list of states would block the improvised actions that tabletop play allows. Decay stops the database from growing forever and gives scribes and historians something to do. Assigning every `settle` timer at once means no AI call is needed later to update an area nobody is watching.

### Boundaries
Technical implementation of world-state storage is defined in [`../architecture/persistence.md`](../architecture/persistence.md).

---

## 3. Playstyles & Economy

### What
Each player has 1 active character, and Session frequency, Session timing, travel time, and rests follow [`time_model.md`](time_model.md). A Session is either a scheduled Campaign Mode party Session arranged through an out-of-game schedule matcher, or a solo or random **One-Shot** triggered by an in-world NPC.

| Mode | Allowed Activities | Restrictions |
|---|---|---|
| **Campaign Mode** | Full AI DM interaction, Dynamic Bubble Turns, PvP anywhere (never opt-in, and policed only by in-world consequences such as witnesses, guards, bounties, and history records, all judged by the AI DM), Hybrid Action Bar and Freeform Action Prompt, and map alterations. | Characters ending mid-combat or in danger are locked out of Downtime until resolved. |
| **Downtime** | Standard 5e Downtime activities (shopping, crafting, and running shops), live barter through real-world notifications when a Campaign Mode party approaches, and spectating Turn Bubbles. | No Freeform Action Prompt, no map alterations, no PvP, and soft collision (cannot body-block or barricade outside owned shops). |

Players may remain in **Infinite Downtime** forever as Level 1 "NPC" shopkeepers or townspeople, and may post quests based on an in-world need that can be verified. Downtime is available anywhere, limited to activities that fit the character's location, such as crafting at a wilderness camp, with shopping only in settlements. Other players can interact with Downtime players socially and economically, such as by talking, trading, and quests, but never through PvP.

Proximity voice chat is a core feature. Communication over long distances uses in-world methods such as letters.

### Why
Every action that changes the story happens inside a Session, and Downtime still lets players spend time in the world between Sessions. Infinite Downtime makes Level 1 crafters and Campaign Mode adventurers depend on each other economically.

### Boundaries
Downtime players cannot use the Freeform Action Prompt, alter world geography, or engage in PvP.

---

## 4. Combat, Rules & Stakes

### What
Exploration is real-time. Combat and tense encounters trigger **Dynamic Bubble Turns**, which create a **Turn Bubble**: a local turn-based area that other Campaign Mode parties can join mid-combat and that Downtime players can only spectate. Looting is a standard action, so the characters closest to the loot reach it first. Time inside a Turn Bubble is defined in [`time_model.md`](time_model.md) Section 5.

The full game uses the 5e rules, with strict classes, visible dice rolls, and levels 1–20, where the AI DM controls leveling and high-level characters are very rare. Leveling progress is hidden: players see their current level and what each level grants, but no progress bar or experience total, and the AI DM decides when a character levels up based on its accomplishments and announces it in the story.

Death is **hard permadeath** unless in-world magic such as *Revivify* or *Raise Dead* revives the character; the character's grave, gear, and deeds stay in world history, and the player creates a new character. Death follows the standard 5e rules, including death saving throws and stabilizing. There is no target death rate, and the AI DM judges danger only by realism. The same stakes apply in every Session, including One-Shots.

When a party member is absent at the start of a Session, the party either declares that the character "slept in" safely or votes unanimously to have the character AI-piloted, meaning the AI DM plays it based on its past actions and pauses for the present players to make any life-or-death choice for it. A character covered as "slept in" or AI-piloted has used its Session for that Calendar Day.

If **3/4 or more** of a party voluntarily disconnects during an active Turn Bubble for **20 real-world minutes**, their characters are permanently deleted. The server measures disconnects. Outages of the game server, the AI provider, or Steam pause the Turn Bubble and do not count toward this rule. Postponing a Turn Bubble under [`time_model.md`](time_model.md) Section 5 is not a disconnect.

Any player, including a Downtime player who disconnects to escape an encounter, can be reported for combat logging. Punishment requires substantial evidence and applies only to repeated offenses.

### Why
Real-time exploration keeps the world moving, and turn-based combat gives combat the tactics of 5e. Permadeath keeps the stakes high, and deleting the characters of a party that walks out of combat stops players from logging out to escape. Outages do not count, so only players who leave on purpose are punished. The unanimous vote and the pauses for life-or-death choices protect absent players.

### Boundaries
Players cannot use logout mechanics to pause or escape active Turn Bubbles. Temporary prototype rule omissions are isolated in [`../prototypes/realism_prototype.md`](../prototypes/realism_prototype.md).

---

## Open Questions
1. **Downtime Gameplay:** Downtime is most of a player's time in the world; what makes it engaging, revisited after the prototype?
