# Review and merge work

This page helps you check an agent's finished work and decide what to keep.

Work moves to **In review** only when the agent has finished and saved its changes. If an agent says it is done but saved nothing, the work moves to **Failed** instead.

## Review the work

1. On the board, click the card under **In review**. Or open it from the inbox on your phone.
2. Read the **Overview**. For a piece of a goal, it starts with your project lead's plain report once every piece is checked; the builder's own notes (commits, commands, file names) are behind **Show details**. **Details** shows what you asked, in your own words, and the brief your project lead wrote for the builder. **Activity** shows every step. A very long summary is kept in full, or cut at the end of a line with a note that the end is missing, never in the middle of a word.
3. Choose one:
   - **Accept** if you are happy with it. The work moves to **Done**, and Decurion sends it to GitHub for you. See the next part. Typing "yes", "push" or "do it" in the message box does the same, as long as the agent did not ask you a question.
   - Pieces of a goal can skip this step. When the app's rule **Checked work goes to GitHub by itself** is on (it is on for new apps), your approval of the plan counts as Accept, so a piece the checker passes goes to GitHub by itself. Its **Overview** says so: "It went to GitHub by itself because you approved the plan", with a link to the goal and the goal's step bar (see [Follow each piece to live](give-your-first-goal.md#follow-each-piece-to-live)). With that rule off, each piece waits for your **Accept**. One that changes a protected part of the app says **Held for you** and waits for your **Accept** either way. Work you started with **Just do it** always waits for you.
   - While a piece of a goal waits for its check, it shows no **Accept**, **Send back** or **Reject**, and typing "yes" does not accept it. It says "Nothing to do. The checker looks at it first." The buttons come back once the check has finished.
   - **Send back** if it needs more. The agent runs again and picks up where it left off. To say what to change, type it under **Tell the agent something** and click **Send back with this answer** instead.
   - **Reject** if you do not want it. Click **Reject work** to confirm. The work moves to **Failed**, and its changes stay on its branch.

If the agent's summary lists problems it found along the way, tap **Fix what it found**. It opens the "What do you want?" box filled in with that summary, so you can start new work from it. The work you are looking at stays as it is.

## After you accept: merge it on GitHub

When you tap **Accept**, Decurion puts the work on your app's GitHub page as a new **branch**, a separate line of changes. Its name starts with `decurion/`, then the app, the number and a few words from the title, such as `decurion/shop-7-fix-login`. It uses this PC's own GitHub sign-in, the same one you use to push from this PC yourself, so you are not asked anything.

Decurion only ever adds a new branch. It never changes your app's main branch, never overwrites a branch that is already there, and never merges by itself. Merging is always yours: on GitHub, or, for an app whose rules are set to **Full control**, with one tap on **Merge** in Decurion (see [Merge from Decurion](#merge-from-decurion) below).

1. Wait a moment. The card says **Sending to GitHub…** while it works.
2. When it is done, you get a notice on your phone, and the card shows a button:
   - **Open pull request** if Decurion opened the pull request for you. Look it over on GitHub, wait for the checks to pass, then click **Merge pull request** there.
   - **Open the pull request** if it did not. The card sits under **Needs you**, says why no pull request was opened, and the tap asks Decurion to open it now. If GitHub still won't let it, **Open on GitHub** opens a page with the pull request filled in: click **Create pull request** there. With a saved GitHub token, Decurion finds that pull request by itself within a few minutes and keeps watch on it like one it opened, so the goal still moves on to **Merged** and closes by itself.

The pull request says what was done, what the checker ran and found (each finding marked checked or guessed), and any protected parts that were held for you. It also says that Decurion made it.

A **research** piece changes no code, so it has no Accept. Tap **Fix what it found** to open the app's Roadmap, where its findings wait as to-dos: tap the **×** next to any you don't want (**Take off the Roadmap**), then **Fix all that are left** (or **Fix this** when only one is left). **Mark done** closes it. Nothing goes to GitHub. See [Research, Ask why, and the checker's proof](research-and-ask-why.md).

## Someone else reviews: Decurion keeps watch

If someone reviews an app's work for you, put their GitHub name in the app's settings under **Who reviews its pull requests**. Each pull request then asks them for a review and says "param reviews next".

After a pull request is open, Decurion looks at it about every five minutes until it is merged or closed. It needs a saved GitHub token for this (see below); without one it does not watch.

- **When it is merged or closed,** you get a notice, and the app's line on the board says so. Decurion stops looking at it then.
- **When someone leaves comments,** you get a notice and an inbox card like "param left 3 comments · Answer the comments". Your own comments do not count.
- **Tap Answer the comments** to start new work that answers those comments. The agent gets the comments as notes from other people to weigh, never as orders. It starts from the work already in the pull request.
- **Review the fix like any other work.** When you accept it, Decurion adds it to the same pull request, on the same branch, and asks the reviewer to look again. It only ever adds on top of what is there: it never rewrites the branch, never opens a second pull request and never merges.

Only one fix per pull request runs at a time. New comments that arrive while it runs wait for the next **Answer the comments**.

## Bring a pull request up to date with main

While you wait for a review, other work may be merged into main. Then the pull request can fall behind main, or conflict with it, and GitHub will not merge it until that is fixed. Decurion reads this from GitHub when it looks at the pull request. The app's line on the board and your phone says **Conflicts with main** or **Behind main**, and the goal page and the work's own panel say **Needs you · bring it up to date**, with **Bring up to date** as their button. You never need GitHub to fix it.

- **Tap Bring up to date** to start new work on that pull request. Decurion gets the newest main from GitHub, and the agent merges it into the pull request's work, fixes any conflicts (keeping what main does now and adding this work on top), and runs the app's own checks and fixes what fails. If Decurion cannot reach GitHub for the newest main, nothing starts and it says so.
- **Follow it from the goal.** While the agent works, the goal says **Bringing it up to date**. When it is ready, it says **Needs you · look over the update**, and its button opens that work.
- **Review it like any other work.** When you accept it, Decurion adds it to the same pull request, on the same branch. It only ever adds on top of what is there: it never rewrites the branch, never changes main and never merges.
- **Protected parts.** If the result changes a part of the app you protect, compared with main, the work says **Held for you** and names the files. Look it over before you tap Accept.

The button shows on any open pull request Decurion opened, whenever no other fix or update is running on it, so you can bring it up to date whenever you like. If it already has the newest main, Decurion says **Already up to date** and starts nothing. This work always runs on your PC.

### Fix conflicts with main by itself

You can have Decurion start **Bring up to date** for you when a pull request conflicts with main. Turn on **Fix conflicts with main by itself** in the app's rules (Settings, Apps, then the app). It is off for every app until you turn it on.

- **Only for conflicts.** It starts when GitHub says the pull request conflicts with main. A pull request that is only behind main waits for your tap.
- **Once for each version it sends.** Decurion tries once for each new version of the pull request it sends to GitHub. If that try fails or you reject it, Decurion waits for you: it does not try again by itself, even when main changes, until it sends a new version. A **Bring up to date** you tapped yourself on that version counts as the try. You can still tap **Bring up to date** yourself at any time.
- **You still accept it.** While the work runs, the board and your phone say "Decurion is fixing the conflicts with main by itself". When it is ready, it waits for your review like any other work and says it was started by Decurion. Nothing goes to GitHub until you tap **Accept**.
- **Same rules as your tap.** It starts only when no other fix or update is running on the pull request, and only when the pull request does not already have the newest main. It uses the same plan and spending limits as work you start.

## Merge from Decurion

For an app you set to **Full control** (Settings, Apps, then the app), a pull request Decurion opened shows a **Merge** button on the board's Today row and on your phone. One tap merges it on GitHub. Nothing merges by itself, and no agent can tap it.

Decurion merges only when GitHub says the pull request is ready: its checks passed, any review it needs is in, and nothing conflicts. Otherwise it tells you what it waits for, and you can still merge on GitHub. It merges exactly the version Decurion sent: if someone else added to the pull request since, merge it on GitHub instead.

To merge, your GitHub token (Settings, GitHub) needs **Contents: Read and write** for that repo, as well as **Pull requests: Read and write**.

Apps set to **PR only**, which is how every app starts, have no Merge button: you or your reviewer merge on GitHub.

## Let Decurion open the pull request for you

This is optional. Without it, you make each pull request yourself with one tap.

1. On GitHub, open **Settings**, then **Developer settings**, then **Personal access tokens**, then **Fine-grained tokens**, and click **Generate new token**.
2. Under **Repository access**, choose your apps' repos.
3. Under **Permissions**, set **Pull requests** to **Read and write**. It needs nothing else. The same token lets Decurion ask your reviewer to look and watch for their comments.
4. Click **Generate token** and copy it.
5. On the board, open **Settings**, then **GitHub**. Paste it into **GitHub token** and click **Save token**.

Decurion keeps the token in this PC's password store and shows only its last four characters. It is never given to an agent. To stop using it, click **Remove** next to it.

If you use the GitHub command line tool (`gh`) and are signed in to it, Decurion uses that sign-in when no token is saved.

## If sending to GitHub does not work

Your work is still accepted, and nothing is lost. The card says **Not sent to GitHub**, with the reason in plain words. Fix what it says, then tap **Try again**. Common reasons:

- **This PC is not signed in to GitHub with access to this app's repo.** Push once from this PC yourself, for example with GitHub Desktop, so it remembers you. Then tap **Try again**.
- **A branch with that name is already on GitHub with other changes.** Decurion did not overwrite it. Delete or rename that branch on GitHub, then tap **Try again**.
- **Could not reach GitHub.** Check this PC's internet connection, then tap **Try again**.
- **This app's folder is not linked to GitHub.** It has no remote named `origin`. Get the work by hand instead, as shown below.
- **The pull request was not opened, but the work was sent.** The card moves to **Needs you**, its step bar stops at **Pull request**, and it says why under it. Fix what it says, then tap **Open the pull request**, or tap **Open on GitHub** to make it yourself. If it says the token was not accepted, save a new one in **Settings**, **GitHub**. If it says your token can't open pull requests on this app, **Settings**, **Sign-ins** shows GitHub amber with the steps to fix it. See [Check your sign-ins](check-your-sign-ins.md).
- **"Can't automatically merge."** GitHub shows this when the work changes the same lines as something merged into the main branch since the work started. You can still open the pull request; it just can't be merged with one click yet. Once it is open, tap **Bring up to date** in Decurion: an agent merges the newest main in and fixes the clash on the same pull request. See [Bring a pull request up to date with main](#bring-a-pull-request-up-to-date-with-main).

## Get the changes by hand

You can always get the work without GitHub. The agent's changes are also saved on a branch in Decurion's own folder.

1. Open the Decurion data folder. On Windows, it is `%LOCALAPPDATA%\decurion`. On a Mac or Linux, it is the `decurion` folder in your home folder.
2. Open the `review` folder. Each finished piece of work has a folder named after the app and a number, such as `portal-7.git`. To find the newest one, sort the folder by **Date modified**.
3. Open Command Prompt and go to your app's own folder.
4. Run `git pull` followed by the path to that review folder and the branch name `decurion/thread-` plus the same number. For example:

   ```text
   git pull "%LOCALAPPDATA%\decurion\review\portal-7.git" decurion/thread-7
   ```

5. Check the result, then push it to GitHub the way you usually do.

Use the copy in the `review` folder, not the one in `workspaces`. Decurion makes the review copy itself, so the agent could not change how it behaves.

## If something goes wrong

- **The work failed with "agent said it finished but saved no commits."** The agent did not save its changes. Click **Retry**, or add new work with a clearer task.
- **There is no folder for my work in `review`.** The agent has not finished yet.
- **`git pull` says the path does not exist.** On a Mac or Linux, use the path to the `decurion` folder in your home folder, such as `~/decurion/review/portal-7.git`.
