# Library Documentation

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | How people and agents look up third-party crate interfaces, and how library knowledge is recorded |
| **Last Updated** | `2026-11-22` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This document defines where the truth about a third-party crate comes from when code is written against it. Bevy and replicon change their interfaces in every release, so memory and web pages describe the wrong version. The downloaded source at the version pinned in `Cargo.lock` is read instead, through 1 offline lookup tool.

---

## 1. Reference Source

### What
The only reference for a third-party crate is its published source and doc comments at the exact version in [`Cargo.lock`](../../Cargo.lock). Cargo stores that source under `~/.cargo/registry/src` after a fetch. Code is never written from memory, tutorials, or web documentation without checking the item in that source first.

### Why
Pre-`1.0` crates such as `bevy` and `bevy_replicon` rename, deprecate, and remove items between minor versions. An agent trained on an older release writes code that looks right and does not compile, or compiles against a deprecated path. The locked source cannot disagree with the compiler.

### Boundaries
Does not choose which crates are allowed; [`code_style.md`](code_style.md) Section 11 owns that.

---

## 2. Lookup Tool

### What
[`tools/lib_docs.ps1`](../../tools/lib_docs.ps1) prints crate information from the locked source. It works offline and inside the sandbox.

| Command | Prints |
|---|---|
| `tools/lib_docs.ps1` | Direct dependencies, locked versions, and source folders |
| `-Crate <name>` | README, crate-level docs, public modules and re-exports |
| `-Crate 'bevy*' -Item <Name>` | Every definition with that name and its doc comment, library source first |
| `-Crate <name> -Find <regex>` | Matching lines with file and line number |
| `-Crate <name> -Examples` | Example files shipped with the crate |
| `-Crate <name> -Changelog` | Newest changelog entries, when the crate ships 1 |

`-Crate` accepts wildcards, and `bevy*` covers the `bevy` facade and every `bevy_*` crate it re-exports. People who prefer a browser run `cargo doc --open --offline`, which renders the same locked source.

### Why
1 command that reads local files is faster than browsing, needs no network, and always matches the build. Agents can run it without approval.

### Boundaries
Does not download sources. A crate missing from the registry folder needs a bypassed `cargo fetch`, run by the Game Director or the coordinating agent.

---

## 3. Agent Workflow

### What
Before using a third-party item, an agent looks it up with the tool and cites the file and line in its report. When the tool shows that an item is deprecated, the agent uses the replacement named in the deprecation note. A library behavior that surprised an agent and cost a failed build is recorded as 1 line under Library Pitfalls in [`AGENTS.md`](../../AGENTS.md).

Every brief given to a subagent or swarm worker that touches third-party code names this document and the tool.

### Why
Citing the source makes a guessed interface visible in review. Recording pitfalls stops the next agent from repeating the same failed build.

### Boundaries
Does not apply to the Rust standard library, which is stable across releases.

---

## 4. Upgrades

### What
Before a dependency version changes, the changelog of every version between the old and new release is read. The tool prints it when the crate ships 1; otherwise the upstream repository release notes are read with network access. Library Pitfalls entries for that crate are checked again after the upgrade and removed when they no longer apply.

### Why
Breaking changes are listed in changelogs before they show up as compile errors across the codebase. Stale pitfalls mislead agents as much as missing ones.

### Boundaries
Does not decide when to upgrade; that follows the dependency rules in [`code_style.md`](code_style.md) Section 11.
