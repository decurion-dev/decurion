# Back up and restore

This page helps you keep a copy of Decurion's saved data and bring it back later.

> **Not built yet.** Decurion has no **Back up** button, and it does not make backups by itself. To keep a copy, copy its data folder by hand, as shown below.

## What is in the data folder

Decurion keeps everything it saves in one folder:

- On Windows: `%LOCALAPPDATA%\decurion`
- On a Mac or Linux: the `decurion` folder in your home folder

The folder holds:

- `hub.db` and files that start with `hub.db`: your board, apps, goals, plans, activity logs, spending and paired phones.
- `keys.json`: secret keys that let Decurion's parts talk to each other.
- `vapid.key`: the key Decurion uses to send notifications to your phone.
- `context`: the files you attached for agents.
- `checkpoints`: saved work, so an agent can pick up where it left off.
- `workspaces`: each agent's copy of your code.
- `review` and `repos`: Decurion's own copies of your apps' code and of finished work.
- `deploys`: the copies and records of your deploys.
- `sign-ins`: your saved website sign-ins, locked.
- `logs`: records of what Decurion did, which help when something goes wrong.
- On a Mac or Linux only, `vault`: your Claude sign-ins, test keys and GitHub token.

On Windows, your Claude sign-ins, pay-per-use key, test keys, GitHub token and the key that unlocks your website sign-ins are **not** in this folder. They stay in Windows Credential Manager. On a Mac or Linux, the pay-per-use key is in your terminal, not in the folder.

## Back up

1. Close Decurion. Work that was running picks up where it stopped when you start Decurion again.
2. Open File Explorer, type `%LOCALAPPDATA%` in the address bar, and press Enter.
3. Copy the `decurion` folder to your backup drive.
4. Start Decurion again.

On a Mac or Linux, close Decurion and copy the `decurion` folder from your home folder instead.

Keep the backup private. `keys.json` and `vapid.key` are secret, the folder holds your code, and on a Mac or Linux the `vault` folder holds your sign-ins.

## Restore

1. Close Decurion.
2. Open `%LOCALAPPDATA%` in File Explorer, as above.
3. If a `decurion` folder is already there, rename it to `decurion-old`.
4. Copy the `decurion` folder from your backup into `%LOCALAPPDATA%`.
5. Start Decurion again. Work that was running goes back to **Queued** and picks up where it left off.

Paired phones keep working after a restore on the same PC.

## Move to a new PC

1. Install Decurion on the new PC. See [Install Decurion](install.md).
2. Restore the folder, as above.
3. Put each app's folder at the same path as before, such as `C:\Users\you\code\portal`. Decurion looks for your apps there.
4. Sign in to Claude again. See [Connect your Claude accounts](connect-claude-accounts.md).
5. Save your pay-per-use key, test keys and GitHub token again. See [Give an app its test keys](add-test-keys.md) and [Review and merge work](review-and-merge.md#let-decurion-open-the-pull-request-for-you).
6. Sign in to each website again. See [Save website sign-ins](save-website-sign-ins.md).
7. Set up Tailscale again and check that your phone can reach the PC. See [Connect your phone](connect-your-phone.md).

## Keep the data somewhere else

To keep the data in a different folder, set `DECURION_DATA` to that folder's full path before you start Decurion.

1. Close Decurion.
2. Move your existing `decurion` folder to the new place. If you skip this, Decurion starts fresh.
3. On Windows, pick a folder inside your own user folder, such as `C:\Users\<you>\decurion-data`. Avoid the top level of another drive, like `D:\`, where other people who use the PC could read it.
4. Open Command Prompt and type `setx DECURION_DATA C:\Users\<you>\decurion-data`, then press Enter.
5. Start Decurion again.

## If something goes wrong

- **Home is empty after a restore.** Decurion may be reading a different folder. Check whether `DECURION_DATA` is set.
- **Your browser says "Not signed in."** Close Decurion and open it again, then click **Sign in** on the page that opens.
- **Website sign-ins don't work after a move to a new PC.** The key that unlocks them stayed on the old PC. Sign in to each website again.
