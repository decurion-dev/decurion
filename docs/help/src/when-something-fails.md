# When something fails

This page helps you find out why work failed and get it going again.

## Find out what happened

1. On the board, look in the **Failed** column. On your phone, failed work shows as **Stopped**.
2. Click the card. The activity log shows what the agent did, step by step.
3. Look at the last lines of the log. The list below explains what they mean.
4. Fix the cause if you can.
5. Click **Retry**. The work goes back to **Queued**, and the agent picks up where it left off.

## What the log tells you

| The log ends with | What to do |
| --- | --- |
| "Agent's box started" and nothing after it | The sealed workspace could not run. Check that Docker Desktop is running, then click **Retry**. |
| FINISHED failed: could not start docker | Decurion cannot find Docker. Install Docker Desktop, then start Decurion again. |
| FINISHED failed: nothing saved in Credential Manager as decurion/claude-profile-A | Save the token for that account. See [Connect your Claude accounts](connect-claude-accounts.md). |
| FINISHED failed: git clone ... | Decurion could not copy your app's folder. Check the app's folder path. See [Add your first app](add-your-first-app.md). |
| FINISHED ok, but the card is in Failed | The agent said it was done but saved no changes. Click **Retry**, or add new work with a clearer task. |
| FINISHED failed, with a message from the agent | The agent hit a problem it could not solve. Read its message, then click **Retry** or add new work with a clearer task. |

## If your PC restarts or Decurion stops

You do not lose work. Decurion saves every step as it happens.

When Decurion starts again, it stops any workspaces that were left behind. Work that was running goes back to **Queued**, and the agent picks up where it left off.

## Other problems

- **"another Decurion runner is already running on this PC."** Decurion is already open in another window. Close the extra copy.
- **The board says "Not signed in."** Close Decurion and open it again, then click **Sign in** on the page that opens. See [Install Decurion](install.md).
- **The board shows an error and nothing updates.** Decurion may have stopped. Look at the Decurion window for a message, then start Decurion again.
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

- Work in **Queued**: click the card, then **Cancel**.
- Work in **Needs you**: click the card, then **Stop**.

Stopped or cancelled work moves to **Failed**. You can still retry it later.

If you need more help, ask on [GitHub](https://github.com/decurion-dev/decurion/issues). Include the last lines of the activity log and of `hub-errors.log` and `runner-errors.log`. Never include your tokens or keys.
