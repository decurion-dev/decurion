# Review and merge work

This page helps you check an agent's finished work and decide what to keep.

Work moves to **In review** only when the agent has finished and saved its changes. If an agent says it is done but saved nothing, the work moves to **Failed** instead.

## Review the work

1. On the board, click the card under **In review**. Or open it from the inbox on your phone.
2. Read the activity log. It shows what the agent did, step by step, and its final summary.
3. Choose one:
   - **Accept** if you are happy with it. The work moves to **Done**.
   - **Send back** if it needs more. The agent runs again and picks up where it left off. Today you cannot add a note when you send work back.
   - **Reject** if you do not want it. The work moves to **Failed**.

> **Coming soon.** This part of Decurion is still being built. This page shows how it will work.
>
> You will see the changed files side by side, along with a second agent's review notes. Decurion will then ask: "Merge pull request into main? Tests passed." Tap **Merge**, and Decurion pushes and merges the work on GitHub for you.

## Get the changes into your app today

Accepting work does not change your app's own folder yet. The agent's changes are saved on a **branch**, a separate line of changes in Git. To bring them into your app, you or someone who uses Git can merge that branch.

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
