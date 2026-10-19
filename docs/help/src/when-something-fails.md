# When something fails

This page helps you find out why work failed and get it going again.

## Find out what happened

1. On the board, look in the **Failed** column. On your phone, failed work shows in the inbox, marked **Failed**.
2. Click the card. The activity log shows what the agent did, step by step.
3. Look at the last lines of the log. The list below explains what they mean.
4. Fix the cause if you can.
5. Click **Retry**. The work goes back to **Queued**, and the agent picks up where it left off.

## What the log tells you

When work fails, the last line of its log says **Could not finish**, followed by the reason.

| The log says | What to do |
| --- | --- |
| **Started its sealed box** and nothing after it | The sealed workspace could not run. Check that Docker Desktop is running, then click **Retry**. |
| could not start docker | Decurion cannot find Docker. Install Docker Desktop, then start Decurion again. |
| nothing saved in Credential Manager as decurion/claude-profile-A | Sign in to that Claude account. See [Connect your Claude accounts](connect-claude-accounts.md). |
| DECURION_CLAUDE_TOKEN_A not set | On a Mac or Linux, nothing is saved for that Claude account. See [Connect your Claude accounts](connect-claude-accounts.md). |
| a message that starts with git clone | Decurion could not copy your app's folder. Check the app's folder path. See [Change an app's plan, name or folder](change-an-app.md#change-an-apps-name-or-folder). |
| agent said it finished but saved no commits | The agent said it was done but saved no changes. Click **Retry**, or ask again with a clearer task. |
| this app uses test keys, which stay on the PC | The work was on the cloud worker. Move it to your PC and try again. See [Add the cloud worker](add-the-cloud-worker.md). |
| a message from the agent | The agent hit a problem it could not solve. Read its message, then click **Retry** or ask again with a clearer task. |

## If your PC restarts or Decurion stops

You do not lose work. Decurion saves every step as it happens.

When Decurion starts again, it stops any workspaces that were left behind. Work that was running goes back to **Queued**, and the agent picks up where it left off.

While Decurion is stopped, open pages show **Decurion is restarting or updating.** They keep trying quietly and come back by themselves once Decurion answers. If they don't come back in a few minutes, check that Decurion is running on your PC. On a device with no internet, the note says **This device is offline.** instead.

## Other problems

- **"another Decurion runner is already running on this computer"** Decurion is already open in another window. Close the extra copy.
- **The board says "Not signed in."** Close Decurion and open it again, then click **Sign in** on the page that opens. See [Install Decurion](install.md).
- **The board says "Your PC's hub isn't answering. Showing the last known board."** Decurion may have stopped. Your work is safe. Start Decurion again.
- **"Decurion stopped because of a problem."** Look in `hub-errors.log`. See [Decurion's log files](#decurions-log-files) below.

## Decurion's log files

Decurion writes down what it does in log files. They help you, or someone helping you, find out what went wrong.

- On Windows, open File Explorer, type `%LOCALAPPDATA%\decurion\logs` in the address bar, and press Enter.
- On a Mac or Linux, open the `logs` folder inside the `decurion` folder in your home folder (`~/decurion/logs`).

| File | What is in it |
| --- | --- |
| `hub.log` | What the board side of Decurion did. |
| `hub-errors.log` | Problems on the board side. Look here first if Decurion will not start. |
| `runner.log` | What the part that starts agents did. |
| `runner-errors.log` | Problems starting or running agents. |

Each time Decurion starts, it keeps the logs from the start before as `.previous.log` files, such as `hub.previous.log`. Look there if the problem happened before you restarted.

## Stop work you no longer want

1. Click the card.
2. Click the button for its state:
   - Work in **Queued**: **Cancel**.
   - Work that is **Running** or in **Needs you**: **Stop**.
3. Confirm with **Cancel work** or **Stop work**. Click **Keep it** if you changed your mind.

Stopped or cancelled work moves to **Failed**. Anything it saved stays on its branch. You can still retry it later, or dismiss it. See [Dismiss finished work](dismiss-finished-work.md).

If you need more help, ask on [GitHub](https://github.com/decurion-dev/decurion/issues). Include the last lines of the activity log and of `hub-errors.log` and `runner-errors.log`. Never include your tokens or keys.
