# Persistence

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | World-state database, Event Log, state checkpoints, and location indexing |
| **Last Updated** | `2026-11-26` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This document defines how world state and its changes are stored. Every change is recorded with its cause in an append-only Event Log. Every stored change and entity can be queried by location.

---

## 1. Database

### What
World state is stored in PostgreSQL from day one, run locally in Docker on the developer laptop for the prototype. The file `compose.yaml` at the repository root starts PostgreSQL with the PostGIS extension for location queries, pinned to a major version, with credentials read from the local `.env` file and data kept in a named Docker volume. The server connects to PostgreSQL with `sqlx` and applies versioned migration files on startup. If PostgreSQL is misconfigured or unreachable on startup, the server exits with an actionable error instead of silently falling back to memory; volatile in-memory mode requires an explicit opt-in (`ZETA_ALLOW_IN_MEMORY_LOG=1`) and notifies connected players in-game. Runtime Event Log write failures surface to players in-game per [`code_style.md`](code_style.md) Section 7.

### Why
PostgreSQL is the database the full game uses, and it stores both strict tables and flexible JSON data such as free-form tags. Starting on a simpler database would mean rewriting queries and migrating data later, which the Longevity Principle rules out. The `sqlx` library runs asynchronously on `tokio`, applies migration files from the repository, and sends PostGIS spatial queries directly to PostgreSQL.

### Boundaries
Does not define table schemas or hosting beyond the laptop prototype.

---

## 2. Event Log

### What
An append-only **Event Log** is the source of truth for world state. Every applied change is stored with its cause, including the player action, the AI request and response, and the dice roll. Current world state is rebuilt from the Event Log plus periodic state checkpoints.

### Why
With the cause of every change stored, developers can roll the world back, trace a bug to the action that caused it, and replay past events as tests. Most of the history system is a view of the Event Log.

### Boundaries
History recording and decay rules are defined in [`../design/game_design.md`](../design/game_design.md) Section 2.

---

## 3. Location Indexing

### What
Every event and entity stores a **Map Partition** identifier plus grid coordinates. A PostGIS spatial index supports radius and area queries, such as everything that happened within a given distance of a point.

### Why
The server needs to look up what happened around a place. The Map Partition identifier also lets the world be split across multiple servers by Map Partition later.

### Boundaries
Does not define Map Partition size or multi-server partitioning.
