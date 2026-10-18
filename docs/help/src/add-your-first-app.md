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

Your app now shows in the list under **Add an app**. It also shows in the app menu where you add work.

## Change how an app is billed

1. Click **Add an app** to see your apps.
2. Next to the app, click **Switch to pay-per-use** or **Switch to Max A** (or **Max B**).

The change applies to work that starts or resumes after you click.

## Choose which websites agents may use

Agents work in a closed room with one door to the internet. Through it they can reach only the AIs (Claude, ChatGPT and Grok), the package stores where code libraries come from (npm, PyPI and crates.io), and the websites you add for that app. They can never reach your PC, your home network or your other devices.

To let an app's agents use another website:

1. Open **Settings** and find the app under **Apps**.
2. Under **Websites**, type the site, such as `stripe.com`, and click **Add**. An entry like `stripe.com` also covers addresses under it, such as `api.stripe.com`.
3. To take a site off the list, click the **×** next to it.

Changes apply to work that starts or resumes after you save.

If an agent tries a website that is not on the list, the visit is turned away and the agent carries on without it. You are not asked anything. The work's activity shows **Blocked a visit to** that site. If the work needs the site, add it and send the work back.

A website on the list can receive information as well as send it, so add only sites you trust.

## Good to know

- Each piece of work gets a fresh copy of the folder. Your own folder is never changed by an agent.
- The copy starts from the latest saved change (commit) on the branch your folder is on now. Changes that you have not committed are not included.
- You cannot rename or remove an app from the board yet.

## If something goes wrong

- **"app name must be 1-40 letters, digits, - or _"** Remove spaces and other marks from the name.
- **"repo_path must be an absolute path without commas or quotes"** Type the full path, starting with the drive letter, such as `C:\`, or with `/` on a Mac or Linux. Remove any quote marks. Folder names with commas do not work.
- **"an app named portal already exists"** Pick a different name.
- **Work on this app fails right away with a "git clone" message.** The folder is not a Git repository, or the path is wrong. Check the path.

Next: [Choose your rules](choose-your-rules.md).
