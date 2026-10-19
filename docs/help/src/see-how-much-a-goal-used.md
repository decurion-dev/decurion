# See how much a goal used

This page shows where to see how much of your Claude plans a goal used, and what the numbers mean.

## Where to look

- **On a goal:** open the goal and its **Pieces of work** tab. The line under the progress bar says how much of your plan the whole goal used, for example "Plan usage so far: light."
- **On each piece:** a piece's card shows its **Plan usage** (its own work and its checks).
- **On Today:** each app's row on the board and on your phone says how much its agents used today, for example "Light plan usage today."

The word is a rough guide: **light**, **moderate** or **heavy**. It weighs what Claude wrote more than what it read, and what it read back from its cache much less. For your plan's real meters, open **Settings**, **Accounts**.

### The exact numbers, in Pro view

Turn on **Pro view** (at the top of the board, or in **Settings** on your phone) to see the exact token counts instead:

- The goal's line says, for example, "This goal used 1.2M tokens · 94% cached (project lead 80k tokens, pieces 1.1M tokens)."
- A piece's card shows **Used** and **Longest conversation** (how full its agent's memory got).
- Today says, for example, "350k tokens · 91% cached."

## What the numbers mean

- **Tokens** are how Claude counts the words it reads and writes. Your Max plan's weekly and five-hour allowances are spent in tokens.
- **Cached** is the share Claude read back from memory it kept from the turn before, instead of reading it all again. Cached reading is the cheap kind: on pay-per-use it costs about a tenth as much. A high share, such as 90% or more, is normal and good.
- **Longest conversation** is the most the agent had to keep in mind at once. When a conversation gets long, Claude sums up the older part to make room. The card says "summed up once" when that happened.

The numbers come from Claude itself. Work that ran before this update has no split, so it may show fewer tokens or no cache share.

## Comparing with doing it by hand

Decurion runs the same Claude Code on the same Max plans, so one job costs about the same either way. A goal adds a project lead that plans and a checker that reviews each piece, so a goal can use more than doing the same by hand. It saves your own time going back and forth, and redoing work the checks would have caught.

To compare, give the same kind of task to a goal and to Claude on your desktop, then look at the goal's line here and at Claude's own usage page for the same day.

## How Decurion keeps agents lean

Every agent starts with only what it needs: no skills, no add-ons, and no tools a sealed box can't use. That makes each agent's starting instructions about 40% smaller than a plain Claude Code start. The instructions are the same in every box, so a new piece can read them back from Claude's cache instead of afresh.
