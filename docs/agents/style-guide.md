# Style guide for screens

The owner's style guide (in the product plan) sets how every Decurion screen
looks. This page holds the rules you need when you build or change a screen.
They apply to the board, the phone, and every page the hub serves.

## One thing at a time: tabs, never a long stack of boxes

A screen never grows into a long column of boxes the owner has to scroll
through. When a screen has more than one area, each area gets its own tab and
only the open tab shows.

| What the screen holds | Use | Example |
| --- | --- | --- |
| Areas of a page that have little to do with each other | A **side list of tabs** on the PC beside the open one; on the phone a **list to tap into**, with "‹ Back" to the list | Settings (`settings.js`: `TABS`, `select`) |
| Many things of one kind, each with its own page | A **list to tap into** (one row each, with what waits on the owner), then the thing's page with "‹ Back" to the list on the phone and the trail on the PC | Apps (`plan.html`): the list of apps, then one app's page |
| Views of one thing | A **row of tabs** with a blue line under the open one (`D.tabs` in `ui.js`, look `.ptabs` in `ui.css`) | A piece of work: Overview, Activity, Details (`D.openWork`). An app: Roadmap, Board, Pages, Files, each a link (`#app=3&tab=board`). A goal: Conversation, Pieces of work, Files |
| Steps done in order | A **row of step tabs** ("1 Key", "2 Server", "3 Connect") with a **Next** button at the end of each step | Add a cloud worker (`cloud.html`) |

Rules for every tab:

- **The first tab is what the owner needs most**, and it opens first. Questions from the agent and the next thing to do sit there.
- **Each tab has its own link** when it is a place the owner goes to (`#settings/apps`), so the back button, reminders and other buttons can open it directly. Views inside a panel (Overview, Activity) need no link.
- **Hidden tabs stay built.** Hide them with `hidden`; never rebuild them on a switch, so nothing the owner typed is lost.
- **A tab says when it needs the owner**, in words, the alert symbol and amber or red ("1 needs you" on Sign-ins), so nothing waiting is hidden behind a tab.
- **A tab opens at its top.**
- **Taps that send the owner somewhere else open the right tab** (for example "Answer the agent" opens Overview with the cursor in the message box).
- A list of the same kind of item (cards on the board, rows of apps) is fine; it is one area. If it gets long, give it filters (the phone board's state tabs) or **Show more**, not more boxes.

## Look

- **Blue** only for buttons you press and the open tab. **Amber** only for what needs the owner, **red** only for failures, **green** only for finished work.
- Every status shows a **word, a symbol and a color**, never color alone.
- Tap targets are at least **44px** on the phone (24px on the PC); tabs are 44px tall everywhere.
- **No sideways scroll at 360px** wide. A row of tabs that does not fit scrolls inside itself, never the page.
- Text contrast meets WCAG 2.2 AA (the colors in `ui.css` already do).

## Words

Short sentences for someone who does not code: say what happened and what to
do next. Tab names are one or two plain words ("Sign-ins", "Activity").

- **A goal's status is its journey's.** Every screen that shows a goal shows
  `journey.label`, its stages (`journey.steps`) and its `next` line, from the
  hub ([`journey.rs`](../../crates/decurion-hub/src/journey.rs)). Never build
  a goal's status words on a page, so the Goals list, the goal page, the
  board, the phone and the Roadmap always agree. A count of what needs the
  owner comes from `GET /api/needs-you`, never from a page's own rule.
- **Roadmap** is each app's list of goals and to-dos, the first tab of the
  app's page under **Apps**. **Plan** on its own only ever means one goal's
  plan (the pieces the owner approves).
- The four places are **Home**, **Goals**, **Apps** and **Settings**, in that
  order on the PC menu and the phone's tabs. Help is a link out (the PC
  menu, the last row of Settings, the phone's small "?"), never a place.

## Check before you finish

Open the screen at 1440, 700, 390 and 360 pixels wide. Click every tab, use
the back button, open a tab from its link, type in a field and switch tabs
and back, and check nothing scrolls sideways.
