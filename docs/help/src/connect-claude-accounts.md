# Connect your Claude accounts

This page helps you let Decurion's agents use your Claude accounts.

Decurion can use up to two Claude Max plans, called **Claude A** and **Claude B**. It can also use a pay-per-use Anthropic key, which is billed for each use.

Decurion never sees your Claude password. Instead, you make a **token** for each account. A token is a long code that lets Claude Code work on your account. It starts with `sk-ant-`. Keep it private, like a password.

> **Coming soon.** This part of Decurion is still being built. Later, you will click **Sign in** for each account in Decurion's setup. For now, follow the steps below.

## Save a token on Windows

Decurion keeps each token in **Windows Credential Manager**, the safe place where Windows keeps passwords. A helper that comes with Decurion saves it there for you.

1. Open the Start menu and click **Decurion - Save a Claude sign-in**. A window opens.
2. When it says "Type A, B or api", type `A` and press Enter.
3. Press Enter again to open the sign-in page. Sign in with the Claude account you want to use as **Claude A**, and allow access.
4. Back in the helper window, Claude shows a long code that starts with `sk-ant-`. Copy it.
5. When the helper asks you to paste the code, paste it and press Enter. Nothing shows on screen while you paste. That is normal.
6. You see "Saved. Decurion will use it for Claude account A." Press Enter to close the window.

To add **Claude B**, do the same steps again. In step 2, type `B`. In step 3, sign in with your other Claude account. If the browser signs you in as Claude A again, sign out of Claude in your browser first.

If you run Decurion from its source code instead of the installer, use `python spike\wincred.py set A` (or `B`, or `api`) to do the same thing.

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

## Save a token on Mac or Linux

On Mac and Linux, Decurion reads tokens from settings in the terminal, called **environment variables**. Support for the Mac Keychain and Linux key stores is coming soon.

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
- **Work fails with "nothing saved in Credential Manager as decurion/claude-profile-B."** The app uses Claude B, but no token is saved for B. Save one, then click **Retry** on the work.
- **Work fails with "DECURION_CLAUDE_TOKEN_A not set."** On Mac or Linux, the token was not set in the terminal that started Decurion. Set it, then start Decurion again.

Next: [Add your first app](add-your-first-app.md).
