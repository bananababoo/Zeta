# Technical Architecture

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | Bevy workspace, crate boundaries, networking library, hosting, spatial audio, concurrency, visual pipeline, content moderation, and time calculation |
| **Last Updated** | `2026-11-21` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This document defines how the game systems specified in [`../design/game_design.md`](../design/game_design.md) are built. The game is 1 Bevy (Rust) ECS workspace. It runs on a laptop first and is built to scale horizontally. AI orchestration is defined in [`ai_orchestration.md`](ai_orchestration.md), and storage is defined in [`persistence.md`](persistence.md).

---

## 1. Engine, Hosting & Audio

### What
The codebase is 1 **Bevy (Rust)** Cargo workspace with a shared core crate, a client crate, and a headless server crate, and it runs an authoritative server, a persistent database, and an asynchronous AI orchestrator. Core physics and rendering components (`GridPos`, `Collider`, `Visual`, `TurnBubble`) are fixed Rust types, and narrative states that come up during play use free-form ECS components. The prototype runs on the developer laptop, hosting then moves to 1 VPS, and every system is built to scale horizontally later. Proximity voice chat uses an existing spatial audio solution.

The core crate holds every rule shared by the client and server and performs no input or output, and it uses Bevy only behind an optional feature. Server-to-client replication uses `bevy_replicon`. Code conventions, including the exact crate boundaries, are defined in [`code_style.md`](code_style.md).

### Why
With Bevy's ECS, the client and server use the same Rust data structures, so no translation layer sits between them and the compiler checks both sides. An existing spatial audio solution already handles real-time voice, so the project does not write its own codecs.

A core crate without input or output gives the same result for the same inputs, which Event Log replay depends on, and keeping Bevy at its edge limits the work of each Bevy upgrade. `bevy_replicon` replicates ECS state from an authoritative server and lets the transport underneath it be swapped without changing game code.

### Boundaries
Does not implement custom audio codecs or run 1 independent AI agent per NPC. 1 scene-level AI DM handles local adjudication.

---

## 2. Concurrency & Visual Pipeline

### What
Player actions use the **Hybrid Action Bar** and the **Freeform Action Prompt**, both restricted to Campaign Mode. Submitting a Freeform Action Prompt places a `PendingAiResolution` lock on exactly the entities in that request's Snapshot, defined in [`ai_orchestration.md`](ai_orchestration.md) Section 1. Locked entities cannot be changed by anything until the resolution applies, except player characters, which stay free to move. A second Freeform Action Prompt whose Snapshot would include any locked entity is queued and resolved in order against a fresh Snapshot.

If any character the AI DM referenced has moved since the Snapshot, the server sends the AI DM a fresh Snapshot and re-resolves once before rolling. If the character moved again, effects apply to the character's current state. Physical world changes use a matching sprite from the curated library in Section 3. When none fits, they start background AI 2D sprite generation, and Bevy shows a VTT overlay badge at once until the new sprite passes moderation and replaces it.

### Why
The AI DM takes 1–3 real-world seconds to respond, and sprite generation takes 5–15 real-world seconds. The game draws 60 frames per second and cannot pause for either. The lock stops 2 players from changing the same entity while the AI DM is deciding. The queue follows the Canonical Order, so no action is thrown away, and player characters stay free to move so players never wait to walk. The overlay badge shows the change at once, so nobody waits for the sprite.

### Boundaries
Does not lock the entire server or unrelated areas of the map during a `PendingAiResolution`.

---

## 3. Content Moderation

### What
Every AI DM response, including narration and NPC speech, passes an automated content filter before any player sees it, and players can report any AI DM output. Visual changes draw first from a curated, human-approved library of sprites and overlays. A sprite is generated only when nothing in the library fits. It passes an automated image filter before it is shown, can be reported, and joins the library only after human approval.

Player text that tries to produce banned content is handled through the player report system in [`../design/game_design.md`](../design/game_design.md) Section 4. The safeguards disclosed to Steam for content generated live by AI are the output filters and player reports. Proximity voice chat is player content, not AI-generated content, so it is moderated with standard tools: per-player mute and block, plus player reports that can include a short recent audio clip.

### Why
Harmful AI output can come from innocent player text, so the filter checks the output itself. Reusing an approved sprite costs nothing and needs no new review. Human approval keeps unreviewed sprites out of the library. Voice chat is not covered by the Steam AI disclosure, so standard tools are enough.

### Boundaries
Does not choose a filtering service, define the length of audio clips, or define the review process for reports.

---

## 4. Time Calculation

### What
The server is authoritative for time. The World Clock and the Calendar Day are calculated from the current UTC time and are never stored or ticked. Each Area Clock is stored only as the World Clock time when its Turn Bubble started plus 6 in-game seconds per completed round. Clients receive the server's UTC offset on connection, calculate the same values locally, and receive an update from the server only when an Area Clock changes.

### Why
Every server and client calculates the same time from UTC, so they send no time-sync traffic, a restart cannot shift the time, and multiple servers need no shared clock. An Area Clock is a start time plus a round count, so it is exact and small to send.

### Boundaries
The time rules themselves are defined in [`../design/time_model.md`](../design/time_model.md).

---

## Open Questions
1. **Tooling Language:** Which language do project tooling scripts use, decided once the language layout of the game is known?
