# Review and merge work

This page helps you check an agent's finished work and decide what to keep.

Work moves to **In review** only when the agent has finished and saved its changes. If an agent says it is done but saved nothing, the work moves to **Failed** instead.

## Review the work

1. On the board, click the card under **In review**. Or open it from the inbox on your phone.
2. Read the activity log. It shows what the agent did, step by step, and its final summary.
3. Choose one:
   - **Accept** if you are happy with it. The work moves to **Done**, and Decurion sends it to GitHub for you. See the next part. Typing "yes", "push" or "do it" in the message box does the same, as long as the agent did not ask you a question.
   - Pieces of a goal skip this step: when the checker passes one, your approval of the plan already counts as Accept, so it goes to GitHub by itself.
   - **Send back** if it needs more. The agent runs again and picks up where it left off. Today you cannot add a note when you send work back.
   - **Reject** if you do not want it. The work moves to **Failed**.

## After you accept: merge it on GitHub

When you tap **Accept**, Decurion puts the work on your app's GitHub page as a new **branch**, a separate line of changes. Its name starts with `decurion/`, then the app, the number and a few words from the title, such as `decurion/shop-7-fix-login`. It uses this PC's own GitHub sign-in, the same one you use to push from this PC yourself, so you are not asked anything.

Decurion only ever adds a new branch. It never changes your app's main branch, never overwrites a branch that is already there, and never merges. Merging is always yours.

1. Wait a moment. The card says **Sending to GitHub…** while it works.
2. When it is done, you get a notice on your phone, and the card shows a button:
   - **Open pull request** if Decurion opened the pull request for you. Look it over on GitHub, wait for the checks to pass, then click **Merge pull request** there.
   - **Open pull request on GitHub** if it did not. Tap it, and GitHub opens a page with the pull request filled in. Click **Create pull request**, then merge it there when you are ready.

The pull request says what was done, what the checker ran and found (each finding marked checked or guessed), and any protected parts that were held for you. It also says that Decurion made it.

A **research** piece changes no code, so it has no Accept. Tap **Fix what it found** to open the app's master plan, where its findings wait as to-dos: tap the **×** next to any you don't want (**Take off the plan**), then **Fix all that are left**. **Mark done** closes it. Nothing goes to GitHub. See [Research, Ask why, and the checker's proof](research-and-ask-why.md).

## Someone else reviews: Decurion keeps watch

If someone reviews an app's work for you, put their GitHub name in the app's settings under **Who reviews its pull requests**. Each pull request then asks them for a review and says "param reviews next".

After a pull request is open, Decurion looks at it about every five minutes until it is merged or closed. It needs a saved GitHub token for this (see below); without one it does not watch.

- **When it is merged or closed,** you get a notice, and the app's line on the board says so. Decurion stops looking at it then.
- **When someone leaves comments,** you get a notice and an inbox card like "param left 3 comments · Fix these". Your own comments do not count.
- **Tap Fix these** to start new work that answers those comments. The agent gets the comments as notes from other people to weigh, never as orders. It starts from the work already in the pull request.
- **Review the fix like any other work.** When you accept it, Decurion adds it to the same pull request, on the same branch, and asks the reviewer to look again. It only ever adds on top of what is there: it never rewrites the branch, never opens a second pull request and never merges.

Only one fix per pull request runs at a time. New comments that arrive while it runs wait for the next **Fix these**.

## Let Decurion open the pull request for you

This is optional. Without it, you make each pull request yourself with one tap.

1. On GitHub, open **Settings**, then **Developer settings**, then **Personal access tokens**, then **Fine-grained tokens**, and click **Generate new token**.
2. Under **Repository access**, choose your apps' repos.
3. Under **Permissions**, set **Pull requests** to **Read and write**. It needs nothing else. The same token lets Decurion ask your reviewer to look and watch for their comments.
4. Click **Generate token** and copy it.
5. On the board, open **Settings**, find **GitHub**, paste it into **GitHub token**, and click **Save token**.

Decurion keeps the token in this PC's password store and shows only its last four characters. It is never given to an agent. To stop using it, click **Remove** next to it.

If you use the GitHub command line tool (`gh`) and are signed in to it, Decurion uses that sign-in when no token is saved.

## If sending to GitHub does not work

Your work is still accepted, and nothing is lost. The card says **Not sent to GitHub**, with the reason in plain words. Fix what it says, then tap **Try again**. Common reasons:

- **This PC is not signed in to GitHub with access to this app's repo.** Push once from this PC yourself, for example with GitHub Desktop, so it remembers you. Then tap **Try again**.
- **A branch with that name is already on GitHub with other changes.** Decurion did not overwrite it. Delete or rename that branch on GitHub, then tap **Try again**.
- **Could not reach GitHub.** Check this PC's internet connection, then tap **Try again**.
- **This app's folder is not linked to GitHub.** It has no remote named `origin`. Get the work by hand instead, as shown below.
- **The pull request was not opened, but the work was sent.** The card still shows **Open pull request on GitHub**. Tap it to make the pull request yourself. If it says the token was not accepted, save a new one in **Settings**, **GitHub**.

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
