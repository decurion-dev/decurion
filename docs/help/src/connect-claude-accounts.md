# Connect your Claude accounts

This page helps you let Decurion's agents use your Claude accounts.

Decurion can use up to two Claude Max plans, called **Claude A** and **Claude B**. It can also use a pay-per-use Anthropic key, which is billed for each use.

Decurion never sees your Claude password. Claude gives Decurion a **sign-in** for each account instead, which lasts one year. Decurion keeps it in **Windows Credential Manager**, the safe place where Windows keeps passwords, and never shows it or writes it to a log.

## Sign in from the board

1. Open the board and click **Settings**. **Sign-ins** is the first section.
2. Click **Sign in on this PC** next to **Claude A**.
3. Claude's own sign-in page opens in your PC's browser. Sign in with the Claude account you want to use as Claude A, and allow access.
4. Come back to the board. After a moment it says **Signed in**, the light turns green, and the row says when the sign-in runs out.

To add **Claude B**, do the same with **Sign in on this PC** next to Claude B. If the browser signs you in as Claude A again, sign out of Claude in that browser first.

If Claude's page did not open, click **The page didn't open?**. Open Claude's page from there, sign in, then paste the short code it shows and click **Send code**.

A week before a sign-in runs out, your phone gets a reminder. Click **Sign in again** to renew it. See [Check your sign-ins](check-your-sign-ins.md).

## The Start-menu helper (older way)

The **Decurion - Save a Claude sign-in** entry in the Start menu still works. It asks you to type `A`, `B` or `api`, opens the sign-in page, and asks you to paste the long code that starts with `sk-ant-`. Use it for the pay-per-use key below. For Claude A and B, the board is easier, and only the board can tell you when a sign-in runs out.

If you run Decurion from its source code instead of the installer, `python spike\wincred.py set A` (or `B`, or `api`) does the same as the helper.

## Add a pay-per-use key (optional)

Use this if you want an app to run on pay-per-use billing instead of a Max plan.

1. In the [Anthropic Console](https://console.anthropic.com/), create an API key and copy it.
2. Open the Start menu and click **Decurion - Save a Claude sign-in**.
3. Type `api` and press Enter.
4. Paste the key and press Enter. You see "Saved. Decurion will use it for the pay-per-use API key."
5. On the board, open **Spending**. Type a monthly cap in dollars and click **Save**. The cap starts at $100.
6. In the Anthropic Console, set a monthly spend limit for the workspace that the key belongs to. Make it the same amount as your Decurion cap.

Why both? Decurion stops pay-per-use work when the month's total reaches your cap. But an agent holds the key while it works, so the Console limit is your safety net.

Decurion also limits each run. One run can spend at most $10 of what is left this month.

To make an app use the pay-per-use key, see [Add your first app](add-your-first-app.md).

## On Mac or Linux

**Sign in on this PC** works the same on a Mac or Linux. Decurion keeps the sign-in in a private folder in your home folder (`~/decurion/vault`) that only your user can read.

You can still give Decurion a token in the terminal instead, in **environment variables**. A sign-in saved from the board is used first.

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
- **"Claude Code is not installed on this PC."** Install Claude Code, then click **Sign in on this PC** again.
- **Work fails with "DECURION_CLAUDE_TOKEN_A not set."** On Mac or Linux, the token was not set in the terminal that started Decurion. Set it, then start Decurion again.

Next: [Add your first app](add-your-first-app.md).
