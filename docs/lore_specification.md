# Lore Specification

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | The Lore Format for every Lore Entry in `lore/`: folders, identifiers, links, front matter, history, validation, and production use |
| **Last Updated** | `2026-11-19` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This specification defines the Lore Format, the rules every Lore Entry in `lore/` follows. People read Lore, and the game loads it for the AI DM, so every Lore Entry uses the same structure and the validator checks every link. The rules for creating Lore are defined in [`design/world_building.md`](design/world_building.md).

---

## 1. Files & Folders

### What
Lore lives in `lore/` at the repository root, outside `docs/`. Each **Lore Entry** is 1 Markdown file with YAML front matter. `lore/` contains 1 subfolder per Lore Entry type, and the folder is the only record of a Lore Entry's type. Each Lore Entry type is added with Game Director approval when the first Lore Entry of that type is written, using the World Anvil article templates as a reference but not as a fixed list. Folders are named in lowercase `snake_case`.

### Why
People can read and edit Markdown in any text editor, and the YAML front matter gives the game fields it can load. Folders by type keep `lore/` easy to browse when it holds thousands of Lore Entries, and the type is recorded only in the folder, so no second copy of it can disagree. World Anvil's templates show which types other worldbuilders found useful, and the project departs from them where Zeta needs something else.

### Boundaries
Does not define the body sections of each Lore Entry type.

---

## 2. Identifiers & Links

### What
A Lore Entry's identifier is its filename without `.md`: its in-world name in Title Case with spaces and without a leading "The", such as `First Crest` for The First Crest. Identifiers are unique across all of `lore/`, ignoring capitalization. When something is renamed in-world, its file is renamed, and every link to it is updated.

Lore Entries link to each other with `[[identifier]]`, such as `the [[First Crest]]`. A link adds display text, `[[identifier|Display Text]]`, only when the sentence needs a different form of the name. This is a project convention that the validator and the game resolve themselves, so no editor such as Obsidian is required.

### Why
Obsidian shows the filename as the page title and in the file list, so a Title Case filename reads as the name, and most links need no display text. Obsidian updates every link when a file is renamed in it. Windows treats filenames that differ only in capitalization as the same file, so identifiers are compared without capitalization. Links use the identifier and not the folder path, so moving a Lore Entry to another folder breaks no links, and the project resolves its own links, so it does not depend on any 1 editor.

### Boundaries
Links to anything outside `lore/` are not allowed.

---

## 3. Front Matter

### What
Every Lore Entry's front matter has the same 5 fields. In the `event` folder, `began` and `ended` hold years, and in every other folder they hold links to events. No field is ever left empty, and each field without a link or year takes 1 of the words listed below:

| Field | Content |
|---|---|
| `name` | The in-world display name. |
| `subtype` | What kind of thing the Lore Entry is within its type, such as `god` in the Character folder or `continent` in the Geography folder. |
| `parent` | The `[[identifier]]` of the Lore Entry it belongs to on the day the game starts, such as the nation a settlement is in, or `none` for top-level Lore Entries. |
| `began` | On an event, the year it started in the objective calendar, `0` or negative for years before year 1, or `unknown` if the Game Director has not decided it yet. On anything else, the `[[identifier]]` of the event that started it, such as a city's founding or a person's birth, or `always` if it has existed from the start, such as a natural law. Something that came about slowly, such as a language, gets a small event to start it. |
| `ended` | On an event, the year it ended in the objective calendar, `present` if it is still going on when the game starts, or `unknown` if the Game Director has not decided it yet. On anything else, the `[[identifier]]` of the event that ended it, such as a city's destruction or a person's death, or `n/a` if it has not ended when the game starts. |

Each folder has a fixed list of allowed `subtype` values, kept in the table below. A folder or value is added, with Game Director approval, when the first Lore Entry needs it. Related Lore Entries are linked only in the body, never listed in the front matter.

| Folder | Allowed `subtype` values |
|---|---|
| `natural_law` | `field`, `aspect`, `principle` |
| `character` | `god` |
| `event` | `cosmic` |
| `geography` | `universe`, `galaxy` |

Until a Lore Entry type has fixed body sections, the body is a 1-sentence summary followed by free prose.

### Why
The filename already records the identifier and the folder already records the type, so the front matter holds only what nothing else records. World Anvil records the kind of thing an article describes in the same way, and a fixed list lets the validator reject misspelled or invented values. Related Lore Entries are linked only in the body, so each relationship is written in 1 place.

Years are written only on events, and every other Lore Entry gets its years from the events its `began` and `ended` link to, so 2 Lore Entries cannot give different years for the same moment. Every Lore Entry uses the same 5 fields, so the same field means the start or end of anything. No field is left empty, so a reader can tell a deliberate `none` from a field someone forgot to fill in.

### Boundaries
Fixed body sections for each Lore Entry type are defined once a few Lore Entries of that type exist.

---

## 4. History

### What
Each historical event is 1 Lore Entry in the `event` folder, with a summary of what actually happened. A large event has detailed sub-events whose `parent` is the large event, such as battles inside a war, and the large event never lists its sub-events. Each era is 1 Lore Entry in the `era` folder: a named span of years whose `began` and `ended` events mark its start and end. The history of a Place is every event that links to it, in order of the year in each event's `began`.

### Why
An event is written once, so the history of a war, a city, and a nation cannot disagree about it. Sub-events find their large event through `parent`, so a sub-event cannot be listed in 1 place and missing from another. An era takes its years from its events, so the era and its events cannot drift apart.

### Boundaries
Player-created history is recorded in the Event Log, defined in [`architecture/persistence.md`](architecture/persistence.md) Section 2, not in `lore/`.

---

## 5. Validation

### What
A Lore validator runs over all of `lore/`. It fails if any `[[identifier]]` link has no matching file, if any `began` or `ended` outside the `event` folder links to anything but an event, if any `began` or `ended` inside the `event` folder holds a link, if any `subtype` is not on its folder's list, if any field is missing or empty, or if any field holds a word not allowed for that field. It also lists every Lore Entry that nothing links to and warns about near-duplicate identifiers.

### Why
A typo in a filename changes the identifier, so every correct link to that Lore Entry stops resolving and the validator fails. A typo in a new Lore Entry that nothing links to yet causes no failure, so the validator lists such Lore Entries and warns about near-duplicate identifiers.

### Boundaries
The validator is written once the Tooling Language Open Question in [`architecture/technical_architecture.md`](architecture/technical_architecture.md) is resolved.

---

## 6. Production Use

### What
The game loads `lore/`, and the AI DM's inspect tool, defined in [`architecture/ai_orchestration.md`](architecture/ai_orchestration.md) Section 2, looks up Lore Entries by identifier.

### Why
The game loads the same files people write, so the game and the people writing Lore never work from different copies.

### Boundaries
Does not define how Lore is stored or indexed inside the game.

---

## Schemas

### Lore Entry Front Matter
```markdown
---
name: <In-world display name>
subtype: <allowed_subtype>
parent: <"[[Parent Identifier]]" or none>
began: <"[[Event Identifier]]" or always>
ended: <"[[Event Identifier]]" or n/a>
---
```

### Event Lore Entry Front Matter
```markdown
---
name: <In-world display name>
subtype: <allowed_subtype>
parent: <"[[Larger Event Identifier]]" or none>
began: <year or unknown>
ended: <year, present, or unknown>
---
```
