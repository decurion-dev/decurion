# Back up and restore

This page helps you keep a copy of Decurion's saved data and bring it back later.

> **Coming soon.** This part of Decurion is still being built. This page shows how it will work.
>
> You will click **Settings**, then **Back up now**. Decurion will also make a backup each week on its own.

Until then, you can back up by copying one folder.

## What is in the data folder

Decurion keeps everything it saves in one folder:

- On Windows: `%LOCALAPPDATA%\decurion`
- On a Mac or Linux: the `decurion` folder in your home folder

The folder holds:

- `hub.db` and files that start with `hub.db`: your board, apps, work, activity logs, spending and paired phones.
- `keys.json`: secret keys that let Decurion's parts talk to each other.
- `vapid.key`: the key Decurion uses to send notifications to your phone.
- `workspaces`: each agent's copy of your code.
- `review`: Decurion's copies of finished work.
- `logs`: records of what Decurion did, which help when something goes wrong.

Your Claude tokens and your pay-per-use key are **not** in this folder. They stay in Windows Credential Manager. After you restore on a new PC, save them again. See [Connect your Claude accounts](connect-claude-accounts.md).

## Back up

1. Close Decurion. Work that was running picks up where it stopped when you start Decurion again.
2. Open File Explorer and type `%LOCALAPPDATA%` in the address bar, then press Enter.
3. Copy the `decurion` folder to your backup drive.
4. Start Decurion again.

On a Mac or Linux, close Decurion and copy the `decurion` folder from your home folder instead.

Keep the backup private. `keys.json` and `vapid.key` are secret, and the folder holds your code.

## Restore

1. Close Decurion.
2. Open `%LOCALAPPDATA%` in File Explorer, as above.
3. If a `decurion` folder is already there, rename it to `decurion-old`.
4. Copy the `decurion` folder from your backup into `%LOCALAPPDATA%`.
5. Start Decurion again. Work that was running goes back to **Queued** and picks up where it left off.

Paired phones keep working after a restore on the same PC. On a new PC:

- Put each app's folder at the same path as before, such as `C:\Users\you\code\portal`. Decurion looks for your apps there.
- Save your Claude tokens again.
- Set up Tailscale again and check that your phone can reach the PC. See [Connect your phone](connect-your-phone.md).

## Keep the data somewhere else

To keep the data in a different folder, set `DECURION_DATA` to that folder's full path before you start Decurion. On Windows, type `setx DECURION_DATA D:\decurion-data` in Command Prompt, then start Decurion again. Move your existing folder there first, or Decurion starts fresh.

## If something goes wrong

- **The board is empty after a restore.** Decurion may be reading a different folder. Check whether `DECURION_DATA` is set.
- **Your browser says "Not signed in."** Close Decurion and open it again, then click **Sign in** on the page that opens.
