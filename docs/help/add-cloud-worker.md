# Add the cloud worker

A cloud worker is a small server you rent from a hosting company. Your agents can work there while your PC is off or busy. It runs the same sealed agent boxes as your PC, with the same rules.

**What it costs:** about $5 to $25 a month for the server, depending on the company. Claude use is counted the same way as on your PC.

**Before you start:** phone access over Tailscale must already work. The server reaches your PC the same way your phone does.

## Set it up

On the board, press **Add cloud worker**. The page walks you through three steps.

1. **Make a Tailscale key.** Open your Tailscale keys page, press **Generate auth key**, keep the settings as they are, and paste the key into the page. The key stays on that page: Decurion never sends it anywhere.
2. **Create a server.** At a hosting company such as Hetzner or DigitalOcean, create a server with **Ubuntu 24.04, 2 CPUs and 4 GB memory**. On the Decurion page, press **Make setup text**, then **Copy**, and paste it into the hosting company's **User data** (or **Cloud config**) box before you create the server. The setup text works once, within 24 hours. Don't share it.
3. **Wait for it to connect.** The first time takes about 10 to 20 minutes, because the server builds Decurion. The page shows **Connected** with a green mark when it is ready. If nothing happens after 40 minutes, delete the server and start again with a new setup text.

## Move work

Open a card on the board and press **Move to cloud** or **Move to PC**. The agent stops, its work so far is saved, and it carries on from the same point on the other computer. New work can be started in either place.

## What is kept safe

- Agents on the server can reach the internet, but not your PC, your phone or anything else on your Tailscale network.
- They can't reach the server's own settings, the hosting company's private addresses, or the server's secrets.
- Other devices on your Tailscale network can't connect to the server.
- The server gets its code only from your PC and from Decurion's public GitHub page. It updates itself to match your PC.

## Remove it

Open **Add cloud worker** on the board and press **Remove** next to the worker. Work running there goes back to your PC. Then delete the server at the hosting company so you stop paying for it.
