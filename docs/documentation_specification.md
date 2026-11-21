# Documentation Specification

| Field | Value |
|---|---|
| **Status** | `Locked` |
| **Scope** | All repository documentation in `docs/` |
| **Last Updated** | `2026-11-19` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This specification defines the naming, placement, structure, formatting, terminology, change control, and agent workflow rules for every document in `docs/`. Every human and AI agent must follow it exactly so documentation stays consistent across conversations, agents, and years of development. When a situation is not covered here, agents must stop and ask the Game Director instead of inventing a new format.

---

## 1. File Naming & Folders

### What
All documentation and source files use lowercase `snake_case`, never `ALL_CAPS`, except tooling defaults such as `Cargo.toml` and Lore Entry files, which are named by [`lore_specification.md`](lore_specification.md). Filenames use full words or the Filename Form of abbreviations registered in [`glossary.md`](glossary.md). The `docs/` folder uses exactly this top-level structure:

```text
docs/
├── documentation_specification.md
├── glossary.md
├── lore_specification.md
├── design/          # What the game is (rules, player experience, world logic)
├── architecture/    # How it is built (Bevy ECS, infrastructure, database, AI schemas)
└── prototypes/      # Temporary, disposable vertical-slice test scopes
```

### Why
With 1 naming convention, agents never have to guess what a shorthand means, and 2 agents never give the same file different names. The 3 folders keep each file focused, and the File Placement Process in Section 2 decides new filenames, so the tree does not list them in advance.

### Boundaries
Does not pre-define individual filenames inside `design/`, `architecture/`, or `prototypes/`, and does not dictate internal Rust module layout inside `crates/`. Agents must not create new top-level folders inside `docs/` without Game Director approval. Lore lives in `lore/` at the repository root, outside `docs/`, and follows [`lore_specification.md`](lore_specification.md) instead of this specification.

---

## 2. File Placement Process

### What
Whenever a new design or technical decision is made, follow these 6 steps in order to decide where it goes:

| Step | Action | Rule |
|---|---|---|
| **1. Pick the Bucket** | Classify the decision's primary nature. | Player-facing rules, Lore Principles, or gameplay loop -> `docs/design/`.<br>Lore itself -> `lore/`, per [`lore_specification.md`](lore_specification.md).<br>Code, Bevy ECS, database, infrastructure, or AI schemas -> `docs/architecture/`.<br>Temporary or disposable test rules -> `docs/prototypes/`. |
| **2. Scan Existing Files** | Read the header of every `.md` file in that bucket. | Look for a file whose `Scope` already covers the same bounded system. |
| **3. Merge or Split** | Decide whether to update an existing file or create a new one. | **Merge** if the decision belongs to that system and keeps the file focused.<br>**Create** a new `<system_name>.md` only if no existing file fits, or if merging would mix 2 independent systems or push the file past 300 lines. |
| **4. Route Cross-Cutting Rules** | Handle decisions that span design and architecture. | Put the gameplay rule and intent in `docs/design/`, put the technical implementation in `docs/architecture/`, and link them. Never state the same rule in 2 places; link to the owning document instead. |
| **5. Register Terms** | Check every new concept name and abbreviation. | Add new Defined Terms to [`glossary.md`](glossary.md) in the same edit. New abbreviations require Game Director approval before use. |
| **6. Apply Template & Update** | Write the entry. | Use the Section 3 skeleton and Section 4 formatting, then update `Last Updated`, and update `Scope` if the boundary changed. |

### Why
The steps are the same every time, so 2 agents, or 2 conversations months apart, file the same decision in the same place. No fixed file tree is needed in advance.

### Boundaries
Does not decide the content of a decision, only where it is recorded.

---

## 3. File Skeleton

### What
Every `.md` file in `docs/` uses this exact structure, with sections in this exact order. Omit `## Schemas` and `## Open Questions` entirely when they have no entries. The header table uses exactly these 4 fields:

| Field | Allowed Values | Format |
|---|---|---|
| **Status** | `Draft`, `Locked`, or `Deprecated` | In backticks. Meanings and transitions are defined in Section 6. |
| **Scope** | The bounded system, crate, or feature the file owns | Plain text, with code identifiers and paths in backticks. |
| **Last Updated** | Date of the most recent edit | `YYYY-MM-DD` in backticks. |
| **Decision Authority** | Game Director (Project Owner) | Plain text, exactly as shown. |

```markdown
# <Title Case Filename>

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | <Bounded system, crate, or feature> |
| **Last Updated** | `YYYY-MM-DD` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
<2–4 sentences summarizing what this document defines and its primary goal.>

---

## 1. <Topic Name>

### What
<Concrete rules, mechanics, or technical specification.>

### Why
<Design, economic, or technical rationale behind this choice.>

### Boundaries
<Explicit non-goals, edge-case limits, or what this does NOT cover.>

---

## Schemas

### <Title Case Name>
<Tables or language-tagged code blocks.>

---

## Open Questions
1. **<Short Label>:** <Unresolved question, including known options.>
```

The skeleton is governed by these structure rules:

- **Title:** The `#` title is the filename without `.md`, with underscores replaced by spaces, in Title Case, using glossary Display Forms for abbreviations (`game_design.md` -> `# Game Design`, `ai_orchestration.md` -> `# AI Orchestration`).
- **Overview:** Exactly 1 paragraph of 2–4 sentences.
- **Topics:** Numbered `## 1.`, `## 2.`, and onward with no gaps, named in Title Case. Each topic contains exactly `### What`, `### Why`, and `### Boundaries`, in that order, and none may be empty.
- **Schemas:** Each entry is a `###` heading with a Title Case name, never a file path, followed by at most 1 introductory sentence and then tables or language-tagged code blocks.
- **Open Questions:** A numbered list in the form `1. **<Short Label>:** <question>`, containing only questions the Game Director raises or approves. When a question is resolved, delete it and record the decision in the owning topic.
- **Nothing Else:** No other `##` sections, no `####` headings, and no content between the header table and `## Overview` other than the `---` separator.

### Why
Every document has the same sections in the same order, so a reader or agent finds the What, Why, and Boundaries of any topic in the same place. Empty sections are left out, so files hold no placeholder headings.

### Boundaries
Does not restrict Rust doc comments (`///`) or the content of code blocks.

---

## 4. Formatting

### What
- **Heading Hierarchy:** Use `#` once for the title, `##` for `Overview`, numbered topics, `Schemas`, and `Open Questions`, and `###` only for `What`, `Why`, `Boundaries`, and `## Schemas` entries.
- **Separators:** Place `---` on its own line, with 1 blank line above and below, immediately before every `##` heading. Never place `---` anywhere else, including the end of a file.
- **Paragraphs:** Write each paragraph on 1 source line with no hard wrapping, with at most 4 sentences. Separate blocks with exactly 1 blank line, and end every file with a single newline.
- **Lists:** Use `-` for unordered items and sequential numbers (`1.`, `2.`, `3.`) for ordered steps, priorities, and Open Questions, nesting at most 1 level. Use `- **Label:** text` when items need labels.
- **Tables:** Use a table for any structured data where items share 2 or more attributes. The divider row uses plain `---` per column with no alignment colons. The first column is bold, except when its values are code identifiers, which use backticks instead, and `<br>` is the only HTML allowed, used only for line breaks inside table cells.
- **Code:** Put code identifiers, file paths, field names, literal values, and `Status` values in backticks. Tag every code block with a language (`rust`, `markdown`, `json`, `toml`, `sql`, or `text`).
- **Links:** Reference other documents with relative Markdown links whose text is the relative path in backticks, such as [`glossary.md`](glossary.md). Refer to a topic as "Section N" after the link, and never link to a file that does not exist.
- **Paths:** Name only files and folders that exist in the repository. The only exceptions are `lore/` and `crates/`, which are reserved before they exist. Describe planned code by what it does, never by a future file, folder, or module path.
- **Emphasis:** Bold is reserved for header field names, first table columns, list labels, the defining use of a Defined Term in its owning document, and critical rule values such as limits, quantities, and hard prohibitions. Italics are reserved for 5e spell and item names and in-world titles.
- **Numbers & Units:** Write all quantities as digits, use an en dash for ranges (`1–20`), and spell out units. Always qualify time as real-world or in-game, such as 4 real-world hours or 6 in-game minutes.
- **Punctuation & Spelling:** Use `->` for arrows and mappings, an em dash (`—`) for parenthetical breaks, the Oxford comma, and American English spelling.

### Why
Every formatting rule removes a choice that 2 agents could otherwise make differently. Predictable source formatting also keeps diffs small and reviews fast. A path to a file that does not exist makes the code look further along than it is, and it fixes a code layout nobody has decided.

### Boundaries
Applies only to `.md` files in `docs/`, and does not govern text inside code blocks.

---

## 5. Terminology

### What
- **Abbreviations:** Only abbreviations registered in [`glossary.md`](glossary.md) may appear in prose, headings, or filenames; every other word is spelled out in full. Exempt: text inside code spans and code blocks, dice notation (d20, 2d6), proper nouns and product names (Bevy, Roll20, Steam), and file extensions. Latin abbreviations (`e.g.`, `i.e.`, `etc.`) are never allowed; write "for example" or "that is" instead.
- **Defined Terms:** Game and project concepts registered as Defined Terms in [`glossary.md`](glossary.md) are always written exactly as listed, in Title Case, and never replaced with synonyms (write Turn Bubble, never `combat bubble`). Official 5e rule terms keep their 5e names and capitalization (Short Rest, Long Rest, Armor Class).
- **Single-Term Rule:** Pick 1 word per concept and use it everywhere, including running prose, and never switch to a synonym for variety (use `error` every time, never `error` and then `mistake`). Never pair 2 words that mean the same thing in a heading, label, column, or filename (use `## Schemas`, not `## Schema & Interfaces`; use `Rule`, not `Rule / Decision Criteria`).
- **Voice:** State rules in the present tense as direct facts ("A Session ends at camp"), never as plans or wishes ("We will try to end Sessions at camp").
- **Plain Writing:** Write short sentences whose subject is a concrete actor, such as the server, a player, or the AI DM, never an abstraction that "makes", "catches", or "keeps" things. Never use promotional or inflated words (`seamless`, `organic`, `genuine`, `robust`, `ensure`, `guarantee`, `crucial`), `rather than` contrasts, lists padded to 3 items, or closing sentences that restate the point. Never use a term that is not a Defined Term, a 5e rule term, or a word a general reader knows, unless the same document defines it first.
- **Why Sections:** A Why states the actual cause or consequence behind the rule, and never restates the What in other words.

### Why
Banning unlisted abbreviations and synonyms means a reader never has to guess what a word means, and agents in different conversations do not drift toward different words for the same thing. Promotional words, padding, and restated Whys add length but no information, and readers recognize them as signs of AI writing.

### Boundaries
Does not govern third-party library names or standard Rust crate identifiers such as `serde` or `tokio`.

---

## 6. Status & Change Control

### What
Every document carries exactly 1 status, and every change is classified before it is made:

| Status | Meaning | Changed By |
|---|---|---|
| **Draft** | Open to iteration; records the decisions made so far. | Game Director |
| **Locked** | Approved; substantive changes require explicit Game Director instruction. | Game Director |
| **Deprecated** | Retired and not for active implementation. The first Overview sentence states why and links any replacement. | Game Director |

| Change Type | Definition | Rule |
|---|---|---|
| **Editorial** | Fixes typos, broken links, formatting, or terminology alignment without changing meaning. | Allowed in any document without approval. |
| **Substantive** | Adds, removes, or changes a rule, value, or boundary. | Draft: only to record Game Director decisions.<br>Locked: only with explicit Game Director instruction naming the change. |
| **Status Change** | Moves a document between `Draft`, `Locked`, and `Deprecated`. | Game Director only. |

Every change updates `Last Updated`, and deprecated documents are never deleted. If a request conflicts with a Locked rule, agents stop and flag the conflict to the Game Director and never silently override it.

### Why
Agents can fix errors without asking, because Editorial changes cannot change meaning. Any change to an approved rule needs the Game Director, so no agent can rewrite one without notice.

### Boundaries
Does not replace version control history.

---

## 7. Agent Workflow

### What
Every agent creating or editing a `.md` file in `docs/` follows these steps in order:

1. Read this specification and [`glossary.md`](glossary.md) in full.
2. Place the content using the File Placement Process in Section 2.
3. Copy the skeleton from Section 3 exactly; never improvise headings, sections, header fields, or table layouts.
4. Never write agent suggestions, proposals, or invented answers into a document without Game Director approval; raise them in conversation first.
5. Verify every check in the compliance table below before finishing.
6. If this specification does not cover a need, stop and propose an amendment to the Game Director instead of improvising.

| Check | Requirement |
|---|---|
| **Title** | Matches the filename per Section 3. |
| **Header** | 4 fields in order, formatted per Section 3, with `Last Updated` set to today. |
| **Structure** | Overview, then numbered topics that each contain What, Why, and Boundaries; Schemas and Open Questions appear only if non-empty. |
| **Separators** | `---` before every `##` heading and nowhere else. |
| **Terminology** | Only registered abbreviations, exact Defined Terms, and every new term registered. |
| **Links** | Relative, resolving, and formatted per Section 4. |
| **Paths** | Every named file and folder exists, except `lore/` and `crates/`. |
| **Plain Writing** | Concrete subjects, no banned words or patterns, and every Why gives the actual reason, per Section 5. |
| **Duplication** | No rule restated from another document. |
| **Code** | Every code block tagged with a language. |

The temporary checker `tools/check_docs.ps1` automates the Title, Separators, and Paths checks, the Links check for resolving targets, and the Terminology and Plain Writing checks for a fixed list of banned terms. All other checks remain manual. The `docs-compliance` hook in `.agents/hooks.json` runs the checker whenever an agent finishes a turn and returns any failures to the agent. The checker is rewritten once the Tooling Language Open Question in [`architecture/technical_architecture.md`](architecture/technical_architecture.md) is resolved.

### Why
Agents forget rules between conversations and drift from them. Following the same steps and checking the same list at the end catches drift before it is committed.

### Boundaries
Does not define review processes for code in `crates/`.

---

## 8. Longevity Principle

### What
All design and technical decisions must optimize for long-term maintainability, clean system separation, and final product quality. When choosing between a quick shortcut and harder or more tedious foundational work, such as splitting specifications cleanly, enforcing strict schemas, or building proper abstractions, choose the rigorous upfront approach.

### Why
A shortcut taken early has to be worked around by every feature built on top of it. Once the world state, multiplayer networking, and AI orchestration grow, removing the shortcut means rewriting those features too.

### Boundaries
Applies permanently to all files in `docs/design/`, `docs/architecture/`, and `crates/`. Temporary shortcuts are permitted only inside `docs/prototypes/` when explicitly scoped as disposable validation tests, such as skipping 5e rules to test the Freeform Action Prompt loop first.
