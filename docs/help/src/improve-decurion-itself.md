# Improve Decurion itself

This page helps you ask Decurion to fix or improve Decurion.

Decurion can work on its own code, the same way it works on your apps. You add Decurion as an app, give it a goal, and review the work. The Decurion you are running does not change until you decide.

## Get Decurion's code

Agents need a copy of Decurion's code that Git keeps track of. The folder where Decurion is installed is not one, so download the code first.

1. Open [GitHub Desktop](https://desktop.github.com/) and choose **Clone a repository**.
2. Choose the **URL** tab and paste `https://github.com/decurion-dev/decurion`.
3. Pick a folder, such as `C:\Users\you\code\decurion`, and click **Clone**.

## Add it as an app

1. Open the board at <http://decurion.localhost:7420> and click **Settings**.
2. Under **Apps**, find **Add an app**.
3. In **Name**, type `decurion`.
4. In **Repo path**, type the full path to the folder you just made, such as `C:\Users\you\code\decurion`.
5. Under **Claude account**, choose **Max plan A** or **Max plan B**.
6. Click **Add app**.

Now give it a goal, the same way as for any app. See [Give your first goal](give-your-first-goal.md). For example: "When a piece of work fails, show the reason at the top of its card."

## What it can do

- Fix problems you run into.
- Add small features you would like.
- Make the words on the board and in these help pages clearer.

Each piece of work runs in its own sealed workspace, with its own copy of the code, like work on any other app. A checker agent looks at it before it reaches you.

## What it can never do

- **Change the Decurion you are running.** Agents work on a copy. Your Decurion keeps running as it is.
- **Merge its own work.** It can open a pull request, but you always decide what goes in.
- **Weaken Decurion's safety rules to finish a task.** If a task would need that, the agent stops and tells you in its summary.
- **Reach your PC, your other apps or your passwords.** The same walls apply as for every app.

## Protected parts

Some parts of Decurion are what keep you safe: the sealed workspaces, the websites agents may use, how keys are kept, and the spending cap. Decurion's code carries a list of the files for these parts.

If a piece of work changes any of those files, Decurion holds it for you and marks it **Needs you · take a look**. This happens even when the checker passed it. You see which protected files were changed. Look closely before you accept it, or ask someone who knows code to look.

An agent cannot change the list to get around this. Decurion always uses the list as it was before the work started.

You can protect parts of your own apps the same way. Add a file named `protected-paths` inside a folder named `.decurion` in the app, with one file or folder per line, such as `billing/`.

## You stay in charge

1. Review the work as usual. See [Review and merge work](review-and-merge.md).
2. If you are happy with it, tap **Accept**. Decurion sends it to GitHub as a new branch with a pull request, and Decurion's checks run there. It never merges and never changes the main branch.
3. Merge the pull request on GitHub when the checks pass, then get the new code into your Decurion code folder. In GitHub Desktop, click **Fetch origin**, then **Pull origin**.
4. The new version runs only after you update Decurion. Install the new version when it comes out. If Decurion runs from your code folder, restart it from there.

A cloud worker updates itself to match your PC, using the code on Decurion's GitHub page. A change that is only in your own folder does not reach it.

## If something goes wrong

- **Work fails right away with a "git clone" message.** The path is not the folder you cloned. Check the path under **Settings**, **Apps**.
- **A piece of work says Needs you · take a look.** It changed a protected part, or the checker still found problems. Read its notes, then accept it, send it back, or reject it.
- **The agent says it could not run the checks.** The agent's workspace can build and test Decurion, but a few checks (the license check, the help book and a check of the cloud worker page) need tools it does not have. Those run on GitHub when the change goes there as a pull request. If the agent says the build or tests themselves could not run, or the work is not going to GitHub, ask someone who knows code to run Decurion's checks before you merge.

Next: [Review and merge work](review-and-merge.md).
