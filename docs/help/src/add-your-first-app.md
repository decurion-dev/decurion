# Add your first app

This page helps you tell Decurion about an app you want agents to work on.

An **app** in Decurion is a folder on your PC that holds your app's code. The folder must be a Git **repository**, a folder where Git keeps the history of every change.

If your code is on GitHub and not yet on your PC, download it first. You can use [GitHub Desktop](https://desktop.github.com/) and choose **Clone a repository**.

## Add the app

1. Open the board at <http://decurion.localhost:7420>.
2. Click **Add an app**.
3. In **Name**, type a short name, such as `portal`. Use only letters, numbers, `-` or `_`. Spaces are not allowed.
4. In the folder box, type the full path to the app's folder, such as `C:\Users\you\code\portal`. On a Mac or Linux, the path starts with `/`.
   - Easier: click **Choose folder** and pick the folder from a list instead of typing. This works on your phone too: it shows the folders on your PC. Start in **Projects**, **Documents** or another code folder, open folders until you reach your app's folder, then click **Use this folder**. A folder that holds your app's Git history is marked **Git app folder**. If the folder you are in is not one, Decurion says so: open the folder that holds your app's `.git` instead.
   - The list shows only folders, never files, and only inside your Projects, Documents and other usual code folders, and the folders your apps are already in. If your app is somewhere else, type its path.
   - Tip: In File Explorer, right-click the folder and choose **Copy as path**. Paste it, then delete the quote marks at each end.
5. Choose which Claude account this app uses: **A** or **B**.
6. To bill this app to your pay-per-use key instead, check **Pay-per-use instead of a Max account**.
7. Click **Add app**.

Your app now shows in the list of apps above **Add an app**. It also shows in the app menu where you add work.

## Each app has its own page

Open **Settings**, then **Apps**. Each app has one row, with its plan and its rules at a glance, such as **Max plan A**, **PR only** and **Patient data**. Tap a row to open the app's page. Everything about that app is there, one tab at a time:

- **Rules** opens first: what Decurion may do for this app (see the next part), and at the bottom, **Always on, for every app**, the safety walls you can't switch off.
- **Plan and money**: its Claude Max plan, pay-per-use, its own monthly cap and **Go first today**.
- **Websites**: the extra websites its agents may use.
- **Files**: briefs, specs and pictures its agents read.
- **Name and folder**.

Tap **Apps** at the top of the page to go back to the list.

## Rules for each app

Each app's rules say what Decurion may do for it. Agents can never change them. Two buttons set them in one tap:

- **PR only**: Decurion sends the work to GitHub and opens the pull request. Nothing more. You or your reviewer merge on GitHub. **New apps always start at PR only.**
- **Full control**: Decurion may also merge and deploy the app, one tap each. Finished pull requests get a **Merge** button, and merged work gets a **Deploy** button.

You can also set each rule on its own with its switch. Then the app shows **Your own mix**.

- **Merge from Decurion.** Finished pull requests get a **Merge** button on the board and on your phone. One tap merges, and only once GitHub says the checks passed and nothing conflicts. Nothing merges by itself. See [Merge from Decurion](review-and-merge.md#merge-from-decurion).
- **Deploy from Decurion.** After a merge you get a **Ready to deploy** card. One tap runs the app's deploy steps on your PC. Decurion can't turn this on until you save the steps:
  1. In **Deploy steps**, type the commands that deploy the app, one per line, the same ones you'd type in the app's folder. Up to 8. Each line runs as one program, without a shell, so marks like `|`, `&&` or `>` are refused: put each command on its own line instead.
  2. In **Live site**, type the address of the live app, such as `https://app.example.com`. This is optional. **Open site** goes there when a deploy finishes.
  3. Tap **Save and turn on** (or **Save** when Deploy is already on).
- **Checked work goes to GitHub by itself.** A piece of a goal you approved goes up as a pull request once the checker passes it. Turn it off to have each piece wait for your **Accept** instead.
- **Its live version holds patient data.** On for new apps. Agents are told to use made-up data only, never real records. Decurion never sees patient data either way.

When you tap **Full control** and no deploy steps are saved yet, Merge turns on and the deploy steps box opens, asking for them.

## Choose who reviews

You can name one person to review each pull request for an app, such as a teammate.

1. Open **Settings**, **Apps**, then the app.
2. On the **Rules** tab, in **Who reviews its pull requests**, type their GitHub name, such as `param-k`.
3. Click **Save**.

They are asked to review each pull request for this app. To stop asking anyone, empty the box and click **Save**.

## Change how an app is billed

1. Open **Settings**, **Apps**, then the app.
2. On the **Plan and money** tab, click **Switch to pay-per-use** or **Switch to Max plan A** (or **B**).

The change applies to work that starts or resumes after you click.

To change which Max plan it uses, choose **Max plan A** or **B** under **Claude Max plan** and click **Save plan**. A new Max plan applies to new work. Work already running keeps the plan it started with. Decurion never changes an app's plan by itself.

## Change an app's name or folder

1. Open **Settings**, **Apps**, then the app.
2. On the **Name and folder** tab, change its **Name** or its **Folder** (type it, or click **Choose folder**).
3. Click **Save**.

While some of the app's work is running, or paused partway, Decurion keeps the old name and folder, because that work keeps its files under them. Wait until the work finishes (or stop it), then change them. You can change the plan, the reviewer and the rules at any time.

## Let one app go first today

When one app matters most today, click **Go first today** on its page in **Settings**, on the **Plan and money** tab, or the star next to its name in the board's app list. Its waiting work then starts before other apps' work.

The star ends by itself at the end of the day. To take it off sooner, click it again. It changes only the order: the money caps and plan limits still apply.

## Apps take turns

When several apps have work waiting, they take turns: after one app's work starts, the next one goes to the app that has waited longest. One big goal does not hold up your other apps. Within one app, the oldest work goes first.

## Choose which websites agents may use

Agents work in a closed room with one door to the internet. Through it they can reach only the AIs (Claude, ChatGPT and Grok), the package stores where code libraries come from (npm, PyPI and crates.io), and the websites you add for that app. They can never reach your PC, your home network or your other devices.

To let an app's agents use another website:

1. Open **Settings**, **Apps**, then the app.
2. On the **Websites** tab, type the site, such as `stripe.com`, and click **Add**. An entry like `stripe.com` also covers addresses under it, such as `api.stripe.com`.
3. To take a site off the list, click the **×** next to it.

Changes apply to work that starts or resumes after you save.

If an agent tries a website that is not on the list, the visit is turned away and the agent carries on without it. You are not asked anything. The work's activity shows **Blocked a visit to** that site. If the work needs the site, add it and send the work back.

A website on the list can receive information as well as send it, so add only sites you trust.

## Good to know

- Each piece of work gets a fresh copy of the folder. Your own folder is never changed by an agent.
- The copy starts from the latest saved change (commit) on the branch your folder is on now. Changes that you have not committed are not included.
- You cannot remove an app from the board yet.

## If something goes wrong

- **"app name must be 1-40 letters, digits, - or _"** Remove spaces and other marks from the name.
- **"repo_path must be an absolute path without commas or quotes"** Type the full path, starting with the drive letter, such as `C:\`, or with `/` on a Mac or Linux. Remove any quote marks. Folder names with commas do not work.
- **"an app named portal already exists"** or **"Another app is already called portal"** Pick a different name.
- **"Wait until this app's running work finishes, then rename it."** Some of its work is running or paused partway. Let it finish, or stop it, then try again.
- **"Type the reviewer's GitHub name"** Use their GitHub name only, such as `param-k`: letters, digits and single dashes, no spaces.
- **"Step on line 2 uses |, which Decurion does not run"** Each deploy step runs as one program, without a shell. Split it into separate lines, one command each.
- **"Add the deploy steps first"** Deploy from Decurion needs saved steps. Type them in **Deploy steps** and tap **Save and turn on**.
- **"This app isn't here any more."** The app's page was opened from an old link. Tap **Back to Apps** and pick it from the list.
- **Work on this app fails right away with a "git clone" message.** The folder is not a Git repository, or the path is wrong. Check the path.

Next: [Choose your rules](choose-your-rules.md).
