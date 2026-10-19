# Answer requests

This page helps you deal with everything that needs you, on your phone or on the board.

Your **inbox** shows only what needs you. Open it on your paired phone (see [Connect your phone](connect-your-phone.md)), or look at the **Needs you** column on the board. Questions from agents come first.

The inbox, the board's **Needs you** column and the dots beside each app all count the same things:

- work that asks you something, is ready for you to check, failed, or didn't reach GitHub;
- goals that wait on you: a plan to approve, a question from your project lead, finished work to review, or a project lead that got stuck;
- pull requests with review comments or a **Merge** button.

Work still waiting for its checker is not counted: the checker looks first. Notices about Decurion's own accounts and sign-ins show at the top of the inbox but are not counted.

Each card says what kind of request it is, which app it is for, and what the agent said last. Nothing in Decurion ever answers a request for you. A request waits until you answer it.

## Answer a request

1. Open the inbox on your phone, or click the card on the board.
2. Read what the card says and what the agent said.
3. Tap one of the buttons. The table shows which buttons each card has.

| The card says | What it means | Your buttons |
| --- | --- | --- |
| The agent asks | An agent asked you a question, often with choices. When it gave no choices, the panel shows "The agent seems to ask" with the sentences that end in a question mark. | Tap a choice. With several questions, pick one for each, then **Send answers**. |
| Needs you · approve the plan | Your project lead has a plan for a goal. | **Approve**, **Open plan** |
| Needs you · answer a question | Your project lead needs your answer before it can make a plan. | **Answer** |
| In review | The agent finished and saved its changes. | **Accept**, **Send back**, **Reject** |
| Needs you | The work stopped until you decide. | **Continue**, **Stop** |
| Failed | The work could not finish. | **Retry**, **Dismiss** |
| Needs you · account out of usage | A Claude Max plan ran out for now. | **Continue on pay-per-use** |
| Needs you · signed out, or Needs you · sign-in runs out soon | A saved website sign-in stopped working, or soon will. | **Sign in again** |
| Needs you · review comments | Someone left comments on a pull request. | **Answer the comments**, and **Merge** if the app's rules allow it |
| Ready to merge | A pull request is open and waits for you. | **Merge**, **Open on GitHub** |

What each button does:

- **Approve** starts every piece of the plan. That is the only yes a goal needs. A plain "yes" or "go ahead" to your project lead does the same. See [Give your first goal](give-your-first-goal.md).
- **Accept** marks the work **Done**, and Decurion sends it to GitHub as a pull request. A plain "yes" in the message box does the same when the agent did not ask you anything. See [Review and merge work](review-and-merge.md).
- **Send back** gives the work back to the agent. It picks up where it left off.
- **Reject** closes the work and marks it **Failed**. Its changes stay on its branch. You can still retry it later.
- **Continue** lets the work go on.
- **Stop** ends the work and marks it **Failed**. Changes so far are kept.
- **Retry** runs the work again. The agent picks up where it left off.
- **Dismiss** hides failed work you don't want to retry. See [Dismiss finished work](dismiss-finished-work.md).
- **Continue on pay-per-use** moves that plan's work to pay-per-use until the plan is back. See [When an account runs out](when-an-account-runs-out.md).
- **Sign in again** opens the sign-in window. See [Save website sign-ins](save-website-sign-ins.md).
- **Answer the comments** starts new work that answers them, on the same pull request.
- **Merge** merges the pull request on GitHub, once GitHub says the checks passed and nothing conflicts. See [Merge from Decurion](review-and-merge.md#merge-from-decurion).

**Reject** and **Stop** ask you to confirm first.

When you act on a card, it leaves your inbox. To say more than a button can, tap **Tell the agent something**. See [Talk to an agent while it works](talk-to-a-running-agent.md).

## Why work stops and waits for you

- **An agent has a question only you can answer.** It stops and waits in **Needs you**.
- **The month's spending cap is reached.** Raise the cap, or wait for next month. See [When an account runs out](when-an-account-runs-out.md).
- **One run reached its own limit.** The agent says "Stopped at this run's spending limit." Tap **Continue** to give it more of this month's budget.
- **A piece of a goal changed a protected part, or still had problems after two checks.** It says **Needs you · take a look**. See [Improve Decurion itself](improve-decurion-itself.md#protected-parts).

Agents do not stop to ask before single steps, such as installing a code library. They work inside their sealed box instead. See [Choose your rules](choose-your-rules.md).

## See every app at a glance

The **Today** strip shows one row for each of your apps. On the board it sits above the columns. On your phone it is the card at the top of the inbox.

Each row says what that app is waiting on, in a few words, with a label:

| The label says | What it means |
| --- | --- |
| Needs you | Something only you can do. The words next to it say what, such as "Approve the plan", "Answer your project lead" or "2 pieces are ready to check". |
| Working | Agents are building. The row says how far they got, such as "2 of 4 pieces built". |
| With reviewer | A pull request is open on GitHub and the app's reviewer has it. |
| Your merge | A pull request is open and the app has no reviewer. Merge it when you are happy with it. |
| Live | The app's newest deploy finished. See [Deploy your app in one tap](deploy-your-app.md). |
| Done | The last pull request was merged. |
| Quiet | Nothing is going on. |

Apps that need you come first, then apps with work running, then the rest by name.

Click a row on the board to show only that app's work. Click it again to see every app. On your phone, tap a row to open that app on the Board tab.

## If something goes wrong

- **A button "did not go through."** The card shows why. The work may have changed on the PC. Wait a few seconds for the inbox to refresh, then try again.
- **"This phone is signed out."** Pair the phone again from the board. See [Connect your phone](connect-your-phone.md).
