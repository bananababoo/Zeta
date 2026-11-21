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
| Code rules | `docs/architecture/code_style.md` |
| Stack: Bevy, bevy_replicon, PostgreSQL + PostGIS | `docs/architecture/technical_architecture.md` |
| AI DM loop, response schema, provider | `docs/architecture/ai_orchestration.md` |
| Database | `docs/architecture/persistence.md` |

## Commands
| Task | Command | Sandbox |
|---|---|---|
| All checks (run before every commit) | `powershell -ExecutionPolicy Bypass -File tools/check.ps1 -Offline` | sandboxed |
| Auto-format, then check | add `-Fix` | sandboxed |
| Docs only | `powershell -ExecutionPolicy Bypass -File tools/check_docs.ps1` | sandboxed |
| Start Docker and the database | `powershell -ExecutionPolicy Bypass -File tools/dev_up.ps1` | bypass |
| Live AI smoke test | `cargo run -p zeta_server --bin ai_check --offline` | bypass (network) |
| Commit, push | `git commit`, `git push` | bypass (`.git` writes) |

## Environment Facts
- cargo is not on PATH: `C:\Users\olive\.cargo\bin\cargo.exe`. `tools/check.ps1` finds it on its own.
- Docker Desktop lives in `C:\Users\olive\AppData\Local\Programs\DockerDesktop`. The sandbox cannot reach the Docker engine or read `~/.docker`.
- The sandbox has no network. `cargo` needs `--offline` there; adding a dependency needs a bypassed `cargo fetch`.
- The shell cannot elevate to admin. Installers that need admin are run by the Game Director.
- `gh` is not installed. Remote: https://github.com/bananababoo/Zeta.git, branch `main`.
- A Stop hook runs `tools/check_docs.ps1 -Hook`; a turn cannot end while docs fail.

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

## Open Work
Update this list when an item is done or added.
1. Database driver choice and server connection to PostgreSQL.
2. Structured-output schema in `AiRequest` (`response_format.schema`).
3. Retry history: the Gemini provider resends only model text. Thinking models may need thought steps resent. Not yet verified live.
4. Bridge between Bevy systems and the tokio runtime for AI calls.
5. Add `bevy_replicon` when the prototype needs networking.
6. Open questions: Tooling Language, snapshot maximum count.
