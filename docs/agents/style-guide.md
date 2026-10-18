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
| Views of one thing | A **row of tabs** with a blue line under the open one (`D.tabs` in `ui.js`, look `.ptabs` in `ui.css`) | A piece of work: Overview, Activity, Details (`D.openWork`). Master plan: Plan, Pages. A goal: Conversation, Pieces of work, Files |
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

## Check before you finish

Open the screen at 1440, 700, 390 and 360 pixels wide. Click every tab, use
the back button, open a tab from its link, type in a field and switch tabs
and back, and check nothing scrolls sideways.
