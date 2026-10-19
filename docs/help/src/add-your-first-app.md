# Add your first app

This page helps you tell Decurion about an app you want agents to work on.

An **app** in Decurion is a folder on your PC that holds your app's code. The folder must be a Git **repository**, a folder where Git keeps the history of every change.

If your code is on GitHub and not yet on your PC, download it first. You can use [GitHub Desktop](https://desktop.github.com/) and choose **Clone a repository**.

## Add the app

1. Open the board at <http://decurion.localhost:7420>.
2. Click the **+** (**Add an app**) next to **Apps** in the list on the left. Or click **Settings**, then **Apps**. The **Add an app** form is under the list of apps.
3. In **Name**, type a short name, such as `portal`. Use only letters, numbers, `-` or `_`. Spaces are not allowed.
4. In **Repo path**, type the full path to the app's folder, such as `C:\Users\you\code\portal`. On a Mac or Linux, the path starts with `/`.
   - Easier: click **Choose folder** and pick the folder from a list instead of typing. This works on your phone too: it shows the folders on your PC. Start in **Projects**, **Documents** or another code folder, open folders until you reach your app's folder, then click **Use this folder**. A folder that holds your app's Git history is marked **Git app folder**. If the folder you are in is not one, Decurion says so: open the folder that holds your app's `.git` instead.
   - The list shows only folders, never files, and only inside your Projects, Documents and other usual code folders, and the folders your apps are already in. If your app is somewhere else, type its path.
   - Tip: In File Explorer, right-click the folder and choose **Copy as path**. Paste it, then delete the quote marks at each end.
5. Under **Claude account**, choose **Max plan A** or **Max plan B**.
6. To bill this app to your pay-per-use key instead, check **Pay-per-use instead of a Max plan**.
7. Click **Add app**.

Your app now shows in the list of apps. It also shows in the app menu where you ask for work. Every new app starts at **PR only**: Decurion opens pull requests for it and does nothing more. See [Choose your rules](choose-your-rules.md).

## Each app has its own page

Open **Settings**, then **Apps**. Each app has one row, with its plan and its rules at a glance, such as **Max plan A**, **PR only** and **Made-up data only**. Every app starts with **Made-up data only** on, to be safe: its agents are told to use made-up data and never real records. If the app's live version holds no patient or personal data, you can turn it off on the app's **Rules** tab. Tap a row to open the app's page. Everything about that app is there, one tab at a time:

- **Rules** opens first: what Decurion may do for this app. See [Choose your rules](choose-your-rules.md).
- **Plan and money**: its Claude Max plan, pay-per-use, its own monthly cap and **Go first today**. See [Change an app's plan, name or folder](change-an-app.md).
- **Websites**: the extra websites its agents may use. See [Choose which websites agents may use](choose-websites.md).
- **Files**: briefs, specs and pictures its agents read. See [Attach files for agents](attach-files.md).
- **Name and folder**: see [Change an app's plan, name or folder](change-an-app.md).

Tap **Apps** at the top of the page to go back to the list.

## Good to know

- Each piece of work gets a fresh copy of the folder. Your own folder is never changed by an agent.
- The copy starts from the latest saved change (commit) on the branch your folder is on now. Changes that you have not committed are not included.
- You cannot remove an app from Decurion yet.

## If something goes wrong

- **"Use only letters, digits, - or _, up to 40."** Remove spaces and other marks from the name.
- **"Use the full path, like C:\path\to\repo, without commas or quotes."** Type the full path, starting with the drive letter, such as `C:\`, or with `/` on a Mac or Linux. Remove any quote marks. Folder names with commas do not work.
- **"An app named portal already exists"** Pick a different name.
- **Work on this app fails right away with a "git clone" message.** The folder is not a Git repository, or the path is wrong. Check the path on the app's **Name and folder** tab.

Next: [Choose your rules](choose-your-rules.md).
