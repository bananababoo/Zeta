# Code Style

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | Rust style base, toolchain, workspace layout, crate boundaries, naming, types, tunable values, errors, serialized data, code documentation, tests, dependencies, and commits |
| **Last Updated** | `2026-11-22` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This document defines how code in `crates/` is written. It adopts the standard Rust conventions and adds rules for what the standard leaves open. Every rule exists so that code written years apart, by different people or agents, reads as if 1 person wrote it.

---

## 1. Style Base

### What
Formatting follows the Rust Style Guide through `rustfmt` defaults, configured only in [`rustfmt.toml`](../../rustfmt.toml). Naming follows the official Rust naming conventions and the Rust library guidelines. Lints follow Clippy with the pedantic group on, configured in [`Cargo.toml`](../../Cargo.toml).

Before every commit, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` pass with no exceptions. [`tools/check.ps1`](../../tools/check.ps1) runs all 3 plus the documentation rules, and CI runs the same script. A lint is silenced only with `#[allow(lint, reason = "...")]` on the smallest item possible, and the reason explains why the lint is wrong in that place.

### Why
A standard style needs no debate and no maintenance, and every Rust tool and library already uses it. Running the checks before each commit keeps warnings from piling up until nobody reads them.

### Boundaries
Does not apply to Lore, docs, or tooling scripts.

---

## 2. Toolchain & Workspace

### What
The compiler version is pinned in [`rust-toolchain.toml`](../../rust-toolchain.toml), and all crates use the 2024 edition. Upgrading the compiler is its own commit, made only after all checks pass on the new version.

All crates live in `crates/` and are named `zeta_<role>`. The workspace has 3: `zeta_core` for shared logic, `zeta_server` for the headless server, and `zeta_client` for the game client. Dependency versions are declared once in `[workspace.dependencies]`, and lints once in `[workspace.lints]`, which every crate inherits. `Cargo.lock` is committed.

Workspace lints forbid `unsafe` code and deny `unwrap`, `dbg!`, and `todo!`. Missing documentation on public items is a warning, which the checks turn into an error.

### Why
A pinned compiler builds the same code the same way on every machine and in every year. Declaring versions and lints once stops crates from drifting apart. The committed lock file rebuilds an old commit with the exact dependencies it was tested with.

### Boundaries
Does not define release builds, packaging, or hosting.

---

## 3. Crate Boundaries

### What
`zeta_server` and `zeta_client` depend on `zeta_core` and never on each other. `zeta_core` performs no input or output: no network, files, database, system clock, environment variables, threads, randomness, or printing. Callers pass in the current UTC time and every dice result. [`clippy.toml`](../../crates/zeta_core/clippy.toml) in `zeta_core` enforces this.

Where iteration order can affect output, `zeta_core` uses `BTreeMap` and `BTreeSet` and never `HashMap` or `HashSet`. When Bevy is added, `zeta_core` uses it only behind an optional `bevy` feature, deriving engine traits with `cfg_attr`, and all other engine code lives in the server and client.

### Why
The same inputs always give the same result, which Event Log replay and deterministic tests depend on. Bevy changes its interfaces every few releases, so keeping it out of the core rules limits each upgrade to thin engine code. The client and server share the same rule code, so they can never disagree about a rule.

### Boundaries
Does not define the server or client internal module layout.

---

## 4. Naming

### What
A Defined Term from [`glossary.md`](../glossary.md) becomes a type name of the same words, such as `CalendarDay`, `WorldClock`, `Snapshot`, and `TurnBubble`, and code never uses a second name for it. Identifiers use full words, such as `position` and `milliseconds`, not `pos` and `ms`.

The only shortened identifiers allowed are approved abbreviations from the glossary, `id`, standard Rust names such as `Vec`, and names fixed by a documented format. The documented formats are the AI Resolution Schema (`chk`, `fx`, `dc`, `vol`) and the core type names in [`technical_architecture.md`](technical_architecture.md) Section 1 (`GridPos`). Module files are named after what they own, and a module with children uses a file named after the module beside a folder of the same name, never `mod.rs`.

### Why
A reader can search the docs for a type name and find its rules, and an agent never invents a second name for a concept. Full words stay readable to someone new to the code, and the fixed formats keep the AI DM wire format short.

### Boundaries
Does not rename identifiers that come from third-party libraries.

---

## 5. Types & Units

### What
Values that are easy to confuse get their own types. Real-world time is a `UtcTimestamp` in milliseconds since the Unix epoch, in-game spans are an `InGameDuration`, and an in-game time of day is a `TimeOfDay`. Stored entity identity is an `EntityId`, which is different from the per-request `LocalId` the AI DM sees.

A value with rules gets a constructor that checks them and returns a `Result`, such as `D20` and `CalendarEpoch`, so an invalid value cannot exist. Enums are preferred over booleans and strings whenever the set of choices is known. Unit names appear in identifiers when the type does not carry them, such as `hearing_range_squares`.

### Why
The compiler then rejects mixing real-world and in-game time, or passing a local identifier where a stored one belongs. Those mistakes are hard to spot in review and expensive to fix once stored in the Event Log.

### Boundaries
Does not define 5e rule types.

---

## 6. Tunable Values

### What
A number whose final value is still an open question is a named constant in the crate's `settings` module, never a literal in the code that uses it. Each constant's comment says that it is a placeholder and names the open question or document that will replace it. Functions take these values as parameters, often in a settings struct whose `Default` reads the constants, so tests can use other values.

A number the docs have already decided, such as the 4-hour Day Cycle or the 6-second round, is a constant next to the code that applies it.

### Why
All undecided numbers sit in 1 file, so nobody mistakes a guess for a decision, and resolving an open question changes 1 line.

### Boundaries
Does not define how settings are loaded at run time.

---

## 7. Errors & Panics

### What
Library code returns typed error enums built with `thiserror`, 1 enum per kind of failure, and each message says what was wrong and what is allowed. Binaries may use `anyhow` once they need it.

Non-test code never calls `unwrap` and never panics on input it does not control. `expect` is allowed only for invariants the code itself upholds, and its message states the invariant, such as `"a value below 1 day in milliseconds fits in u32"`. Arithmetic on values that come from outside the program uses checked or saturating operations. Tests may use `expect` and `panic!` freely.

### Why
A server that panics drops every connected player. Typed errors make the compiler list every failure a caller must handle. Validation messages also go back to the AI DM on retry, so they have to be exact enough to act on.

### Boundaries
Does not define logging or error reporting to players.

---

## 8. Serialized Data

### What
Every type that is stored in the Event Log or database, sent over the network, or exchanged with the AI DM derives `serde` with explicit field and tag names. Enums use internal tags (`op`, `kind`, or `role`), lowercase or `snake_case`. The internal tag rule covers JSON data; network types sent in the binary format follow the rules in the `zeta_core` protocol module docs instead, because that format cannot read internally tagged enums.

Once a format has been stored or sent, its field names and tags never change and are never removed. A change adds an optional field or a new variant instead. Every JSON example in the docs is copied into a test that parses it.

### Why
The Event Log is kept forever and replayed, so a record written today must still load in 10 years. Testing the documented examples keeps the docs and the code from disagreeing.

### Boundaries
Does not define database table schemas or the network protocol.

---

## 9. Documentation in Code

### What
Every public item has a `///` comment. Every module starts with a `//!` comment saying what it owns and naming the document and section that define its rules.

Comments explain why, never repeat what the code says, and never restate a rule owned by a document. Fallible public functions have an `# Errors` section. Comments follow the writing rules of [`documentation_specification.md`](../documentation_specification.md) where they apply, including Defined Terms and approved abbreviations.

### Why
The docs stay the only place each rule is written, and the code points to them, so the 2 cannot quietly disagree. A reader opening any file learns at once where its rules come from.

### Boundaries
Does not require comments on private items whose names explain them.

---

## 10. Tests

### What
Unit tests live in the same file as the code, in a `#[cfg(test)] mod tests` block at the end. Test names describe a behavior as a sentence, such as `calendar_day_rolls_over_exactly_at_monday_midnight`. Shared test helpers live in `test_support`, compiled only for tests.

`zeta_core` is tested without Bevy, a network, or an async runtime. AI DM behavior is tested by replaying recorded exchanges through `ReplayProvider`, and every bug fix adds a test that fails without the fix.

### Why
Tests beside the code are updated with it. Replayed exchanges run the same way every time and cost nothing, and they show exactly what changes when a provider updates its model.

### Boundaries
Does not define the AI DM scenario test suite or its pass thresholds.

---

## 11. Dependencies

### What
A dependency is added only when it saves substantial protocol, cryptography, or engine work that cannot be written cleanly in a short module with the standard library, and only when it is widely used and actively maintained. Each dependency is declared in the workspace [`Cargo.toml`](../../Cargo.toml) with a comment saying why it is needed, with `default-features = false` whenever the crate bundles optional subsystems so unused features and duplicate backends stay out of `Cargo.lock`. `zeta_core` depends only on `serde`, `serde_json`, `thiserror`, and optional `bevy_ecs` behind the `bevy` feature.

Any pre-`1.0` dependency, such as `bevy_ecs`, `bevy_app`, `reqwest`, or `sqlx`, is quarantined inside the single module that needs it behind Zeta's own types or traits. Game rules and callers outside that module never import its types directly.

### Why
Every external crate and enabled feature can break when it updates or stall compiler upgrades when it is abandoned. Turning off unused features cuts transitive crates and duplicate libraries from the build. Quarantining pre-`1.0` libraries behind Zeta's own interfaces means a breaking library upgrade edits 1 file instead of the codebase.

### Boundaries
Does not choose libraries for a role; the networking library is chosen in [`technical_architecture.md`](technical_architecture.md) Section 1, and the database library is chosen in [`persistence.md`](persistence.md) Section 1.

---

## 12. Commits

### What
Each commit holds 1 logical change and passes every check in Section 1. The summary line is in the imperative mood, such as "Add Area Clock", and is at most 72 characters. A blank line and a body follow when the reason for the change is not obvious from the summary.

### Why
Small commits that each pass the checks can be reverted on their own and searched for the commit that introduced a bug. The summary format matches what git tools display.

### Boundaries
Does not define branching or pushing to a remote.
