# Answer requests

This page helps you deal with work that is waiting for you, on your phone or on the board.

Your **inbox** shows only the work that needs you. Open it on your paired phone (see [Connect your phone](connect-your-phone.md)), or click a card on the board.

Each card says what kind of request it is, which app it is for, and what the agent said last.

## See every app at a glance

The **Today** strip shows one row for each of your apps. On the board it sits above the columns. On your phone it is the card at the top of the inbox.

Each row says what that app is waiting on, in a few words, with a label:

| The label says | What it means |
| --- | --- |
| Needs you | Something only you can do: a plan to approve, a question to answer, or work to check. |
| Working | Agents are building. The row says how far they got, such as "2 of 4 pieces built". |
| With reviewer | A pull request is open on GitHub and the app's reviewer has it. |
| Your merge | A pull request is open and the app has no reviewer. Merge it on GitHub when you are happy with it. |
| Done | The last pull request was merged. |
| Quiet | Nothing is going on. |

Apps that need you come first, then apps with work running, then the rest by name.

Click a row on the board to show only that app's work. Click it again to see every app. On your phone, tap a row to open that app on the Board tab.

When someone left comments on a pull request, the row says so, for example "PR #88 · param left 3 comments", and shows **Fix these**. Tap it and an agent starts new work to answer those comments. It goes to the same pull request, so you review it there as before.

## Answer a request

1. Open the inbox on your phone, or click the card on the board.
2. Read what the card says and what the agent said.
3. Tap one of the buttons:

| The card says | What it means | Your buttons |
| --- | --- | --- |
| Ready for review | The agent finished and saved its changes. | **Accept**, **Send back**, **Reject** |
| Waiting on you | The work stopped until you decide. | **Continue**, **Stop** |
| Stopped | The work could not finish. | **Retry** |

What each button does:

- **Accept** marks the work Done.
- **Send back** gives the work back to the agent. It picks up where it left off.
- **Reject** marks the work Failed. You can still retry it later.
- **Continue** lets the work go on.
- **Stop** ends the work and marks it Failed.
- **Retry** runs the work again. The agent picks up where it left off.

When you accept, send back, continue or retry, the card leaves your inbox. Work you reject or stop stays in the inbox as **Stopped**, so you can retry it later.

## Why work waits on you today

Today, work waits on you when pay-per-use money runs out:

- **The month's spending cap is reached.** Raise the cap first, or wait for next month. See [When an account runs out](when-an-account-runs-out.md).
- **One run reached its own limit.** The agent says "Stopped at this run's spending limit." Tap **Continue** to give it more of this month's budget.

Nothing in Decurion ever answers a request for you. A request waits until you answer it.

> **Coming soon.** This part of Decurion is still being built. This page shows how it will work.
>
> Agents will also ask before risky steps, based on [your rules](choose-your-rules.md). For example: "Agent wants to run: npm install zod." You will tap **Approve**, **Deny**, or **Always allow** for that app.

## If something goes wrong

- **"Accept" did not go through.** The card shows why. The work may have changed state on the PC. Wait a few seconds for the inbox to refresh, then try again.
- **"This phone is signed out."** Pair the phone again from the board.
