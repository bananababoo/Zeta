# Time Model

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | Calendar Day, World Clock, Day Cycles, Session timing, travel time, Turn Bubble time, and event ordering |
| **Last Updated** | `2026-11-16` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This document defines how time passes in the shared world. All players share 1 timeline, and parties in the same area at the same real-world time see the same time of day. Every Session has a natural stopping point, and players lose little by missing a week. The Calendar Day advances once per real-world week, and the faster World Clock drives the visible day and night.

---

## 1. Calendar Day

### What
The **Calendar Day** advances by exactly 1 day per real-world week, rolling over every Monday at 00:00 UTC. All history records, seasons, festivals, and world events are dated by Calendar Day only, never by in-game hour.

A **Session** is 1 AI DM story session, either a Campaign Mode party Session or a One-Shot. Each character takes part in at most **1 Session per Calendar Day**, so every Session is that party's next in-game day. A character who skips a real-world week spent that Calendar Day resting or in Downtime, with no penalty.

Between Sessions, a character is either in Downtime or offline, and it may be in Downtime both before and after its Session within the same Calendar Day. A character in Downtime is visible at its location, and other players can interact with it as defined in [`game_design.md`](game_design.md) Section 3. An offline character is not in the world at all. It is treated as being elsewhere nearby and cannot be found, harmed, or robbed.

### Why
With 1 Session per Calendar Day, every party's story moves at the same pace as the world. Two parties can play the same Calendar Day at different real-world times, and dating events by Calendar Day hides the difference. The cap stops busy players from falling far behind players who grind, keeps AI costs predictable, and removes the pressure to log in daily.

### Boundaries
Does not define history record content or decay, which are defined in [`game_design.md`](game_design.md) Section 2.

---

## 2. World Clock & Day Cycles

### What
The **World Clock** is the single shared time of day for the entire world. It runs through 1 full 24-hour in-game day every **4 real-world hours**, a span called a **Day Cycle**, so 1 real-world minute equals 6 in-game minutes. Each Day Cycle begins at **Dawn** (in-game 06:00) and ends at the following Dawn.

Dawn occurs at 00:00, 04:00, 08:00, 12:00, 16:00, and 20:00 UTC, giving 6 Day Cycles per real-world day and 42 Day Cycles per Calendar Day. Every Day Cycle within a Calendar Day replays that same Calendar Day. Downtime players see the day and night of every Day Cycle.

### Why
The World Clock runs on a fixed schedule, so every party in an area at the same real-world time sees the same time of day. With 6 Dawns per real-world day, every timezone has a Dawn in the evening. Replaying 1 Calendar Day across many Day Cycles is unrealistic, but it keeps the Calendar Day in step with the rule of 1 Session per Calendar Day.

### Boundaries
The World Clock never advances the Calendar Day. Does not define lighting, weather, or seasonal visuals.

---

## 3. Session Timing

### What
A Session that starts at Dawn gets a full in-game day. A Session that starts later begins at the current World Clock time and has a shorter day. A Session ends when the party makes camp or enters Downtime, or at the end of its Day Cycle at the latest, so Sessions normally last at most **4 real-world hours**. The only exception is a Turn Bubble still active at the end of the Day Cycle (Section 5).

A Long Rest takes place between Sessions when the party ends its Session at a safe camp or in Downtime. Inside a Session, in-game time always runs at the World Clock rate. The AI DM **cannot skip or fast-forward time** ahead of the World Clock.

If the Day Cycle ends while no Turn Bubble is active, the Session ends wherever the party is, and the AI DM narrates the party making camp on the spot. The Long Rest takes place only if the AI DM judges the spot safe, and the Downtime lockout applies only if the party is in actual danger.

### Why
A Session ends when the characters stop for the night or when their in-game day runs out, so the stopping point makes sense in the story. The AI DM cannot skip time, so every party's time of day matches the World Clock and parties in the same place always agree on it.

### Boundaries
Characters ending a Session mid-combat or in danger follow the Downtime lockout in [`game_design.md`](game_design.md) Section 3. Does not define the out-of-game schedule matcher.

---

## 4. Travel

### What
Distances between settlements follow the world's geography and Lore and are never shortened for convenience, so long journeys may span many Sessions. Lore states distances as travel time (such as "3 days' ride"). Map distances are scaled so those travel times hold in both real-world and in-game terms at the World Clock rate. Out-of-combat movement speed is set by how it feels to play, not by 5e walking pace, and the 5-foot grid and 5e movement rules still apply inside Turn Bubbles.

Long distances use the **Overland Scale**, a zoomed-out map on which the party crosses terrain faster on screen while in-game time still passes at the World Clock rate. Travel is core content, including encounters, camps, weather, and navigation. Quiet stretches are acceptable. The party spends them talking with each other, looking at the scenery, and interacting with their surroundings, and the time a journey takes makes it feel like actually going somewhere.

### Why
Distances stay the same size, so the map stays consistent for everyone and every party stays on the World Clock. Lore states distance as travel time, so the map scale and movement speed can change without contradicting Lore. Real distance makes trade, news, letters, and differences between places matter.

### Boundaries
Does not define encounter generation, mounts, ships, or magical travel.

---

## 5. Turn Bubble Time

### What
While a Turn Bubble is active, its **Bubble Area** runs on an **Area Clock** that advances exactly 6 in-game seconds per completed 5e round, regardless of real-world time. The Bubble Area is the space the Turn Bubble covers plus the perception range around it (line of sight plus hearing range, the same range that bounds a Snapshot in [`../architecture/ai_orchestration.md`](../architecture/ai_orchestration.md) Section 1). Everyone within the Bubble Area, including spectators, sees the Area Clock. Overlapping Bubble Areas share 1 Area Clock set to the earliest of their times.

When the Turn Bubble ends, **Catch-Up** advances the Area Clock over a few real-world seconds until it matches the World Clock, and the AI DM narrates it as the party catching its breath. If Catch-Up spans 1 in-game hour or more, it counts as a Short Rest for the participants. A character who leaves a Bubble Area before Catch-Up completes switches to the World Clock at once. Catch-Up time is lost travel time, so every combat costs the party daylight and journeys must allow for trouble on the road.

If a Turn Bubble is still active when its Day Cycle ends, the participants either keep going until the combat is over or postpone it to their next Session. A continued Turn Bubble keeps its Area Clock past the end of the Day Cycle, and its Catch-Up then advances to the World Clock of the new Day Cycle. A postponed Turn Bubble is frozen.

A majority vote of all players in the Turn Bubble decides whether to postpone it, and the result applies to every party in it. A frozen Turn Bubble stays visible as a paused scene that nobody can enter or interact with, and it blocks its Bubble Area until it resumes. It resumes at the start of the next Session of any party in it. Absent player characters are AI-piloted if the present members of their party vote unanimously, otherwise the AI DM plays them defensively, and non-player characters continue under the AI DM as usual.

### Why
A 5e round is 6 in-game seconds but takes minutes of real-world time, so on the World Clock the sun would race across the sky during combat. Only the Bubble Area slows down, so the rest of the world stays on the World Clock, and Catch-Up brings the Bubble Area back to it. A 5e Short Rest is 1 in-game hour, so a long Catch-Up counts as one. Players can finish or postpone combat, so the end of a Day Cycle never cuts combat off.

### Boundaries
Does not define turn order or how parties join a Turn Bubble, which are defined in [`game_design.md`](game_design.md) Section 4. An Area Clock never runs ahead of the World Clock.

---

## 6. Canonical Order & Unobserved Areas

### What
Real-world time decides the **Canonical Order** of events. If 1 party changes the world before another party arrives in real-world time, the later party sees that change, regardless of either party's in-game time of day within the Calendar Day. When no connected character could perceive an area, using the same perception range as the Bubble Area, every pending `settle` timer there completes at once to its final state.

### Why
Events are ordered by real-world time, so an event never has to be undone because another party played earlier in the in-game day. Timers in an area nobody can perceive complete at once, so the server never runs timers that nobody can see.

### Boundaries
Settling content and history recording are defined in [`game_design.md`](game_design.md) Section 2.
