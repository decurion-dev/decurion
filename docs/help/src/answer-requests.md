# Answer requests

This page helps you deal with everything that needs you, on your phone or on your PC.

**Home** opens on **Needs you**: only what waits for you, on your PC and on your paired phone (see [Connect your phone](connect-your-phone.md)). Questions from agents come first. **Working** and **Done today** sit beside it.

The **Needs you** tab, the number on **Home** in the menu and the amber count beside each app (on Home and under **Apps**) all count the same things:

- work that asks you something, is ready for you to check, failed, or didn't reach GitHub;
- goals that wait on you: a plan to approve, a question from your project lead, finished work to review, or a project lead that got stuck;
- pull requests with review comments or a **Merge** button.

Work still waiting for its checker is not counted: the checker looks first. Notices about Decurion's own accounts, such as a Claude plan out of usage, show at the top of **Needs you** but are not counted. On your phone, so do website sign-ins that need you to sign in again.

Each card says what kind of request it is, which app it is for, and what the agent said last. Nothing in Decurion ever answers a request for you. A request waits until you answer it.

## Answer a request

1. Open **Home** on your phone or your PC, and tap the request under **Needs you**.
2. Read what the card says and what the agent said.
3. Tap one of the buttons. The table shows which buttons each card has.

| The card says | What it means | Your buttons |
| --- | --- | --- |
| The agent asks | An agent asked you a question, often with choices. When it gave no choices, the panel shows "The agent seems to ask" with the sentences that end in a question mark. | Tap a choice. With several questions, pick one for each, then **Send answers**. |
| Needs you · approve the plan | Your project lead has a plan for a goal. | **Approve the plan**: it opens the goal, where the plan is shown, and you approve it there |
| Needs you · reply | Your project lead needs your answer before it can make a plan. | **Reply** |
| Needs you · review the work | Every piece of a goal is through, and your project lead's report is ready. | **Review the work** |
| Needs you · try again | Your project lead got stuck. | **Try again** |
| In review | The agent finished and saved its changes. | **Accept**, **Send back**, **Reject** |
| Needs you | The work stopped until you decide. | **Continue**, **Stop** |
| Failed | The work could not finish. | **Retry**, **Dismiss** (on a PC, **Dismiss** is in the work's panel: tap the work to open it) |
| Needs you · account out of usage | A Claude Max plan ran out for now. | **Continue on pay-per-use** |
| Needs you · signed out, or Needs you · sign-in runs out soon | A saved website sign-in stopped working, or soon will. | **Sign in again** |
| Needs you · review comments | Someone left comments on a pull request. | **Answer the comments**, and **Merge** if the app's rules allow it |
| Ready to merge | A pull request is open and waits for you. | **Merge**, **Open on GitHub** |

What each button does:

- **Approve the plan** opens the goal with its plan. **Approve plan** there starts every piece of the plan. That is the only yes a goal needs. A plain "yes" or "go ahead" to your project lead does the same. See [Give your first goal](give-your-first-goal.md).
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

When you act on a card, it leaves **Needs you**. To say more than a button can, tap **Tell the agent something**. See [Talk to an agent while it works](talk-to-a-running-agent.md).

## Why work stops and waits for you

- **An agent has a question only you can answer.** It stops and waits in **Needs you**.
- **The month's spending cap is reached.** Raise the cap, or wait for next month. See [When an account runs out](when-an-account-runs-out.md).
- **One run reached its own limit.** The agent says "Stopped at this run's spending limit." Tap **Continue** to give it more of this month's budget.
- **A piece of a goal changed a protected part, or still had problems after two checks.** It says **Needs you · take a look**. See [Improve Decurion itself](improve-decurion-itself.md#protected-parts).

Agents do not stop to ask before single steps, such as installing a code library. They work inside their sealed box instead. See [Choose your rules](choose-your-rules.md).

## See every app at a glance

Each app has one line on **Home**, with a few words on what it waits on and a label. An app that needs you sits at the top of **Needs you**, under **Apps that need you**. The other apps sit at the top of **Working**.

Each line also has that app's buttons: **Run it** (see [Run your app and try it](run-your-app.md)), and, when they apply, **Answer the comments**, **Merge**, **Bring up to date** and **Deploy** (see [Review and merge work](review-and-merge.md) and [Deploy your app in one tap](deploy-your-app.md)).

| The label says | What it means |
| --- | --- |
| Needs you | Something only you can do. The words next to it say what, such as "Approve the plan", "Answer your project lead" or "2 pieces are ready to check". |
| Working | Agents are building. The line says how far they got, such as "2 of 4 pieces built". |
| With reviewer | A pull request is open on GitHub and the app's reviewer has it. |
| Your merge | A pull request is open and the app has no reviewer. Merge it when you are happy with it. |
| Live | The app's newest deploy finished. See [Deploy your app in one tap](deploy-your-app.md). |
| Done | The last pull request was merged. |
| Quiet | Nothing is going on. |

Tap an app's name to show only that app on Home. Tap it again to see every app. With two apps or more, the **All apps** box at the top does the same. Home shows every app each time you open it.

## If something goes wrong

- **A button "did not go through."** The card shows why. The work may have changed on the PC. Wait a few seconds for Home to refresh, then try again.
- **"This phone is signed out."** Pair the phone again from **Settings**, **Phones** on your PC. See [Connect your phone](connect-your-phone.md).
