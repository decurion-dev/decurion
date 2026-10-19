# What is Decurion

This page tells you what Decurion does and where to start.

Decurion is a control room for AI coding helpers, such as Claude Code. We call each helper an **agent**. You say what you want in plain words, and agents write the code for one of your apps. You stay in charge of what gets kept.

Decurion is free and open source. It runs on your own PC. Windows comes first. Mac and Linux work too, with a few extra steps.

## What Decurion does for you

- **Plans with you.** You give a **goal**, such as "Add a password reset page." Your **project lead**, one agent you talk to, asks a few questions and shows you a plan, with pictures of how the app will look. One yes starts the work. See [Give your first goal](give-your-first-goal.md).
- **Builds and checks.** Agents build each piece of the plan. A separate **checker** agent tries each piece and says what it ran and found. See [Research, Ask why, and the checker's proof](research-and-ask-why.md).
- **Keeps agents sealed off.** Each agent works in its own **sealed workspace**, a closed-off space on your PC. It changes only its own copy of your app's code. It cannot see your other files or your passwords, and it reaches only the websites you allow. See [Choose your rules](choose-your-rules.md).
- **Sends work to GitHub.** Finished work goes to your app's GitHub page as a **pull request**, a proposed change you can look at before it goes in. Decurion never changes your main code by itself. See [Review and merge work](review-and-merge.md).
- **Merges and deploys with one tap, if you let it.** For apps you set to **Full control**, you can merge and put the app live from Decurion. See [Deploy your app in one tap](deploy-your-app.md).
- **Works from your phone.** Your phone gets notices, shows what needs you, and does everything the board does. See [Connect your phone](connect-your-phone.md).
- **Lets you try the app.** **Run it** starts your app on your PC and shows it on the board or your phone. See [Run your app and try it](run-your-app.md).
- **Keeps an eye on money.** It shows what your Claude plans and pay-per-use keys used, and stops pay-per-use work at your caps. See [When an account runs out](when-an-account-runs-out.md).

## How the pieces fit

1. You add an **app**. That is a folder on your PC that holds your app's code.
2. You ask for something for that app: a goal for your project lead, or **Just do it** for a small change.
3. Decurion makes a fresh copy of the app's code and starts an agent in a sealed workspace.
4. The agent works and saves its changes in its own copy.
5. When it is done, the work shows up for your review, or, for a goal you approved, goes to GitHub once the checker passes it.

## The words you will see

Every piece of work is in one of these states. You see the same words on the board and on your phone.

| Word | What it means |
| --- | --- |
| Queued | Waiting to start. Your PC works on up to 3 pieces at once. The rest wait here for a free place. |
| Running | An agent is working on it. |
| Needs you | It stopped and is waiting for you to decide something. |
| In review | The agent finished and saved its changes. Look at it and decide. |
| Done | You accepted it. |
| Failed | It could not finish, or you stopped it. You can try again. |

## What is not built yet

- Decurion does not ask you before an agent's single steps, such as installing a code library. See [Choose your rules](choose-your-rules.md).
- There is no backup button. See [Back up and restore](back-up-and-restore.md).
- There is no button to save a ChatGPT or Grok key. See [Connect your Claude accounts](connect-claude-accounts.md#chatgpt-and-grok).
- You cannot remove an app once you add it.

## Where to start

If you are new, follow these pages in order:

1. [Install Decurion](install.md)
2. [Connect your Claude accounts](connect-claude-accounts.md)
3. [Add your first app](add-your-first-app.md)
4. [Connect your phone](connect-your-phone.md)
5. [Give your first goal](give-your-first-goal.md)

## Get more help

You can open these pages any time from **Help** in Decurion's menu, on your PC and on your phone. Decurion shows them itself, in a new tab, with the list of pages on the side; **Back to Decurion** takes you back.

Ask a question or report a problem on [GitHub](https://github.com/decurion-dev/decurion/issues).
