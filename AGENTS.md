# Agent Notes for Zeta

Read this first in every session. It holds what an agent needs to work here without relearning it.
Decisions live in `docs/`; this file only points to them and records working knowledge.

## Project
- Zeta: multiplayer fantasy game on D&D 5e rules with an AI DM. Solo developer, the Game Director.
- The Game Director writes Lore. Agents build Lore-independent code unless asked otherwise.
- **Longevity Principle:** prefer the choice that stays easy to maintain for years over the quick one.
- Never write your own ideas into docs without the Game Director's approval.

## Where Decisions Live
| Topic | File |
|---|---|
| Doc rules (Locked) | `docs/documentation_specification.md`, `docs/glossary.md` |
| Gameplay, 2024 5e (SRD 5.2.1), items & crafting | `docs/design/game_design.md` |
| First playable slice | `docs/prototypes/realism_prototype.md` |
| Code & dependency rules | `docs/architecture/code_style.md` |
| Stack: Bevy, bevy_replicon, client modules | `docs/architecture/technical_architecture.md` |
| AI DM loop, response schema, catalog lookup | `docs/architecture/ai_orchestration.md` |
| Database (`sqlx`, PostgreSQL + PostGIS) | `docs/architecture/persistence.md` |
| Looking up third-party crate interfaces | `docs/architecture/library_documentation.md` |

## Commands
| Task | Command | Sandbox |
|---|---|---|
| Look up a crate at its locked version | `powershell -ExecutionPolicy Bypass -File tools/lib_docs.ps1 -Crate <name> [-Item <Name> \| -Find <regex>]` | sandboxed |
| All checks (run before every commit) | `powershell -ExecutionPolicy Bypass -File tools/check.ps1 -Offline` | sandboxed |
| Auto-format, then check | add `-Fix` | sandboxed |
| Docs only | `powershell -ExecutionPolicy Bypass -File tools/check_docs.ps1` | sandboxed |
| Start Docker and the database | `powershell -ExecutionPolicy Bypass -File tools/dev_up.ps1` | bypass |
| Live AI smoke test | `cargo run -p zeta_server --bin ai_check --offline` | bypass (network) |
| Live DB smoke test | `cargo run -p zeta_server --bin db_check --offline` | bypass (Docker/DB) |
| Live DB integration tests | `cargo test -p zeta_server --offline -- --ignored` | bypass (Docker/DB) |
| Run the server | `cargo run -p zeta_server --offline` | bypass (sockets) |
| Run a client (1 per player) | `cargo run -p zeta_client --offline` | bypass (sockets) |
| Socket tests | `cargo test --workspace --offline --all-features -- --ignored` | bypass (sockets) |
| Commit, push | `git commit`, `git push` | bypass (`.git` writes) |

## Environment Facts
- cargo is not on PATH: `C:\Users\olive\.cargo\bin\cargo.exe`. `tools/check.ps1` finds it on its own.
- Docker Desktop lives in `C:\Users\olive\AppData\Local\Programs\DockerDesktop`. The sandbox cannot reach the Docker engine or read `~/.docker`.
- The sandbox has no network. `cargo` needs `--offline` there; adding a dependency needs a bypassed `cargo fetch`.
- The shell cannot elevate to admin. Installers that need admin are run by the Game Director.
- `gh` is not installed. Remote: https://github.com/bananababoo/Zeta.git, branch `main`.
- A Stop hook runs `tools/check_docs.ps1 -Hook`; a turn cannot end while docs fail.
- The sandbox denies loopback connects (os error 10013), so socket tests are `#[ignore]` and run bypassed with `cargo test --workspace --offline --all-features -- --ignored`.
- Server and client addresses come from `ZETA_LISTEN_ADDRESS` and `ZETA_SERVER_ADDRESS` (see `.env.example`).

## Library Rules & Pitfalls (Bevy 0.20, bevy_replicon 0.45)
- Never write a library call from memory. Check it first with `tools/lib_docs.ps1` and cite file:line. Every brief to a subagent or worker repeats this.
- Bevy 0.20: a `Resource` is also a `Component`; deriving both on 1 type conflicts. That is why `MapArea(MapBounds)` exists.
- Bevy 0.20: `Interaction` and `Button` are deprecated; the client hit-tests clicks with `ComputedNode`.
- Bevy has feature collections (`2d`, `ui`, `2d_api`); enable those, not the default feature set.
- replicon: `SendMode` is a deprecated alias of `SendTargets` (`All`, `AllExcept`, `Single`). `ClientId` is an enum `{ Client(Entity), Server }`.
- replicon: `add_client_message` / `add_server_message` take a `Channel` (`Unreliable`, `Unordered`, `Ordered`). Clients are authorized through the protocol check (`AuthorizedClient`).
- replicon sends with postcard, which cannot decode internally tagged serde enums. Network types are structs and unit-only enums.
- `bevy_replicon_renet` has no Bevy 0.20 release; Zeta uses its own TCP backend.
- Network protocol: types live in `crates/zeta_core/src/protocol/` by domain. All registration stays in 1 append-only `register_protocol`, because registration order is each type's network number. Read the `protocol.rs` module docs before adding a type.

## PowerShell Pitfalls
- Read `$LASTEXITCODE`, not stderr. cargo and docker write progress to stderr on success.
- A backtick inside double quotes is an escape. Use single quotes for literal text.
- Scripted file writes must use UTF-8 without BOM and LF: `[IO.File]::WriteAllText(path, text, [Text.UTF8Encoding]::new($false))`.
- Nested quotes in `sh -c` arguments get mangled. Pipe SQL or scripts through stdin.
- `Select-String` has no `-Recurse`. Use `Get-ChildItem -Recurse | Select-String`.

## Doc Writing Checklist
- Update **Last Updated** on every edit. Digits for numbers. At most 300 lines.
- `---` before every `##`. Sections use What, Why, Boundaries.
- The checker bans filler words and unapproved abbreviations; it prints the exact line to fix.
- Backticked paths containing `/` must exist.

## Roadmap & Open Work
Update this list as milestones finish or new work is added. Each milestone builds additively as independent Bevy plugins + replicated ECS components (`docs/architecture/technical_architecture.md` Section 5).

1. **Milestone 1 — Realism Prototype (`docs/prototypes/realism_prototype.md`):** built; awaiting a Game Director playtest.
   - Server ECS world, `Snapshot` from live components, d20 roll, `WorldEffect` execution, and `EventLogStore` writes.
   - Add `bevy_replicon` multiplayer state replication between `zeta_server` and 2+ `zeta_client` instances.
   - Core client plugins only: 2D grid + WASD/click movement, Freeform Action Prompt, Turn Bubble banner, visible d20 roll, and shared narration/speech log.
2. **Milestone 2 — 2024 5e SRD 5.2.1 Item Catalog & Inventory (`docs/design/game_design.md` Section 5):**
   - Import official CC-BY-4.0 SRD 5.2.1 equipment/tools catalog into `zeta_core` (never pull non-SRD book content such as `5e.tools`; add CC-BY-4.0 attribution).
   - Add replicated `Inventory` + client item cards with generic verbs (`Equip`, `Drop`, `Give`, `Consume`, deterministic `Craft`, and `Utilize` -> pre-attached Freeform Action Prompt).
   - Validate AI-awarded `catalog_id`s in `zeta_core::validation`, add narrative-only custom item escape hatch (no combat stats or crafting recipes), and expose catalog lookup via the AI `inspect` tool.
3. **Milestone 3 — Character Sheet HUD & Hybrid Action Bar (`docs/design/game_design.md` Section 4):**
   - 2024 5e stats, HP/temp HP/death saves, AC, conditions, proficiencies, and spell slots/resources HUD.
   - 1-click standard 5e actions (`Attack`, `Dash`, `Dodge`, `Hide`, `Search`, spells, weapon attacks).
4. **Milestone 4 — Downtime Crafting, Shops & Economy (`docs/design/game_design.md` Section 3):**
   - Zero-AI deterministic crafting window, player/NPC shops, barter UI, and Campaign/Downtime mode rules.
5. **Milestone 5 — Moderation, Sprites & Voice (`docs/architecture/technical_architecture.md` Sections 1–3):**
   - Automated AI output filter + player report button, curated sprite library + background sprite generation, and proximity voice controls.
6. **Open Questions:** Tooling Language (`technical_architecture.md`), Snapshot Maximum Count (`ai_orchestration.md`), Test Suite Thresholds (`realism_prototype.md`), Downtime Gameplay (`game_design.md`).
