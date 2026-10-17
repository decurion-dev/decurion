# Add the cloud worker

This page helps you let agents work in the cloud, so work does not stop when your PC is off or busy.

A cloud worker is a small server you rent from a hosting company. It runs the same sealed agent boxes as your PC, under the same rules. See [Choose your rules](choose-your-rules.md).

**What it costs:** about $5 to $25 a month for the server, depending on the company. Claude use is counted the same way as on your PC.

**Before you start:** phone access over Tailscale must already work, because the server reaches your PC the same way your phone does. See [Connect your phone](connect-your-phone.md).

## Set it up

On the board, open **Settings**. Under **Computers**, click **Add cloud worker**. The page walks you through three steps.

1. **Make a Tailscale key.** Open your Tailscale keys page, click **Generate auth key**, keep the settings as they are, and paste the key into the page. The key stays on that page: Decurion never sends it anywhere.
2. **Create a server.** At a hosting company such as Hetzner or DigitalOcean, create a server with **Ubuntu 24.04, 2 CPUs and 4 GB memory**. On the Decurion page, click **Make setup text**, then **Copy**. Paste it into the hosting company's **User data** (or **Cloud config**) box before you create the server. The setup text works once, within 24 hours. Don't share it.
3. **Wait for it to connect.** The first time takes about 10 to 20 minutes, because the server builds Decurion. The page shows **Connected** with a green mark when it is ready. If nothing happens after 40 minutes, delete the server and start again with a new setup text.

## Choose where work runs

Once a cloud worker is set up, the box where you describe new work has a choice: **On your PC** or **In the cloud**. Each card on the board shows where its work runs.

To move work, open its card and click **Move to cloud** or **Move to PC**. The agent stops, its work so far is saved, and it carries on from the same point on the other computer. It does not ask you anything new.

## What is kept safe

- Agents on the server can reach the internet, but not your PC, your phone or anything else on your Tailscale network.
- They can't reach the server's own settings, the hosting company's private addresses, or the server's secrets.
- Other devices on your Tailscale network can't connect to the server.
- The server keeps no Claude sign-in or API key of its own. Your PC lends it the one a piece of work needs, only while that work runs there, and a pay-per-use key never enters an agent's box.
- The server gets its code only from your PC and from Decurion's public GitHub page. It updates itself to match your PC.

## Remove it

On the board, open **Settings**. Under **Computers**, click **Remove** next to the cloud worker. Work running there comes back to your PC and carries on. Then delete the server at the hosting company so you stop paying for it.
