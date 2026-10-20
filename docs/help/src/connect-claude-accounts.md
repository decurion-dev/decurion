# Connect your Claude accounts

This page helps you let Decurion's agents use your Claude accounts.

Decurion can use up to two Claude Max plans, called **Claude A** and **Claude B**. It can also use a pay-per-use Anthropic key, which is billed for each use.

Decurion never sees your Claude password. Claude gives Decurion a **sign-in** for each account instead, which lasts one year. On Windows, Decurion keeps it in **Windows Credential Manager**, the safe place where Windows keeps passwords. On a Mac or Linux, it keeps it in a private folder (see below). It never shows it or writes it to a log.

## Sign in from Decurion

1. Open Decurion on your PC and click **Settings** in the menu. **Sign-ins** is the first tab.
2. Click **Sign in on this PC** next to **Claude A**.
3. Claude's own sign-in page opens in your PC's browser. Sign in with the Claude account you want to use as Claude A, and allow access.
4. Come back to Decurion. After a moment it says **Signed in**, the light turns green, and the row says when the sign-in runs out.

To add **Claude B**, do the same with **Sign in on this PC** next to Claude B. If the browser signs you in as Claude A again, sign out of Claude in that browser first.

You can also tap the button on your phone. The page still opens on your PC, so do this when you are at your PC.

If Claude's page did not open, click **The page didn't open?**, then **Open Claude's sign-in page**. Sign in, copy the code Claude's page shows, paste it into **Code from Claude's page** and click **Send code**.

To change which account a plan uses, click **Sign in again** next to it.

A week before a sign-in runs out, your phone gets a reminder. Click **Sign in again** to renew it. See [Check your sign-ins](check-your-sign-ins.md).

## The Start-menu helper (older way)

The **Decurion - Save a Claude sign-in** entry in the Start menu still works. It asks you to type `A`, `B` or `api`, opens the sign-in page, and asks you to paste the long code that starts with `sk-ant-`. Use it for the pay-per-use key below. For Claude A and B, **Settings** is easier, and only **Settings** can tell you when a sign-in runs out.

If you run Decurion from its source code instead of the installer, `python spike\wincred.py set A` (or `B`, or `api`) does the same as the helper.

## Add a pay-per-use key (optional)

Use this if you want an app to run on pay-per-use billing instead of a Max plan.

1. In the [Anthropic Console](https://console.anthropic.com/), create an API key and copy it.
2. Open the Start menu and click **Decurion - Save a Claude sign-in**.
3. Type `api` and press Enter.
4. Paste the key and press Enter. You see "Saved. Decurion will use it for the pay-per-use API key."
5. In Decurion on your PC, open **Settings**, then **Accounts**. Next to **Claude pay-per-use**, type a **Monthly cap** in dollars and click **Save cap**. The cap starts at $100.
6. In the Anthropic Console, set a monthly spend limit for the workspace that the key belongs to. Make it the same amount as your Decurion cap.

Why both? Decurion stops pay-per-use work when the month's total reaches your cap. But an agent holds the key while it works, so the Console limit is your safety net.

Decurion also limits each run. One run can spend at most $10 of what is left this month.

To make an app use the pay-per-use key, see [Change an app's plan, name or folder](change-an-app.md).

## ChatGPT and Grok

**Just do it** work can also go to ChatGPT or Grok. These are billed by use, under their own caps in **Settings**, **Accounts**. Each starts at $25 a month.

> **Not built yet.** There is no button or Start-menu helper to save a ChatGPT (OpenAI) or Grok (xAI) key. Today only someone who runs Decurion from its source code can save one, with `python spike\wincred.py set openai` or `python spike\wincred.py set xai`. On a Mac or Linux, they can set `DECURION_OPENAI_API_KEY` or `DECURION_XAI_API_KEY` in the terminal instead.

## On Mac or Linux

**Sign in on this PC** works the same on a Mac or Linux. Decurion keeps the sign-in in a private folder in your home folder (`~/decurion/vault`) that only your user can read.

You can still give Decurion a token in the terminal instead, in **environment variables** (named settings that a terminal passes to the programs it starts). A sign-in saved in Decurion's **Settings** is used first.

The pay-per-use key works only this way on a Mac or Linux. There is no Start-menu helper there.

1. In a terminal, run `claude setup-token`. Sign in with the Claude account you want as Claude A, then copy the code that starts with `sk-ant-`.
2. In the terminal where you will start Decurion, type `read -rs DECURION_CLAUDE_TOKEN_A && export DECURION_CLAUDE_TOKEN_A` and press Enter.
3. Paste the token and press Enter. Nothing shows while you paste.
4. Run `decurion-start` in that same terminal.

Use these names:

| Account | Name |
| --- | --- |
| Claude A | `DECURION_CLAUDE_TOKEN_A` |
| Claude B | `DECURION_CLAUDE_TOKEN_B` |
| Pay-per-use key | `DECURION_ANTHROPIC_API_KEY` |

You need to do this each time you open a new terminal to start Decurion.

## If something goes wrong

- **"That does not look like a Claude sign-in code" or "That does not look like an Anthropic API key".** You copied something else, or picked the wrong letter. A Max account code starts with `sk-ant-oat`. A pay-per-use API key starts with `sk-ant-api`. Copy the right one and try again.
- **"Claude Code is not installed."** The helper cannot open the sign-in page. Install Claude Code, or run `claude setup-token` on another PC and paste that code.
- **Work fails with "nothing saved in Credential Manager as decurion/claude-profile-B."** The app uses Claude B, but nothing is saved for B. In Settings, Sign-ins, click **Sign in on this PC** next to Claude B, then click **Retry** on the work.
- **"Claude Code is not installed on this PC."** Install Claude Code (the Windows installer offers it), then click **Sign in on this PC** again.
- **"A Claude sign-in is already open."** Finish it in the browser, or click **Cancel** at the top of **Sign-ins**, then start again.
- **"Claude Code didn't start its sign-in on this PC."** Decurion waits up to 45 seconds for Claude's page. Update Claude Code, then click **Try again**. If the message ends with what Claude said, that line tells whoever helps you what went wrong.
- **"The sign-in took too long."** Decurion waits 10 minutes. Click **Try again**.
- **Claude B signs in as your Claude A account.** Your browser is still signed in to Claude A. Sign out of Claude in that browser, then click **Sign in on this PC** next to Claude B again.
- **Work fails with "DECURION_CLAUDE_TOKEN_A not set."** or **"DECURION_ANTHROPIC_API_KEY not set."** On a Mac or Linux, it was not set in the terminal that started Decurion. For Claude A or B, click **Sign in on this PC** in **Settings**, **Sign-ins** instead. Or set it, then start Decurion again.

Next: [Add your first app](add-your-first-app.md).
