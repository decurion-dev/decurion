# Connect your phone

This page helps you get Decurion's inbox on your phone, with notifications.

Your phone reaches your PC through **Tailscale**, a free app that makes a private network between your own devices. Nobody else can reach Decurion this way.

## Set up Tailscale (once)

1. Install Tailscale on your PC and on your phone. Sign in to the same Tailscale account on both.
2. On your PC, open Command Prompt. Type `tailscale serve --bg 7420` and press Enter.
3. If Tailscale asks you to turn on HTTPS for your network, follow the link it shows and turn it on. Then run the command again.
4. Tailscale shows an address that starts with `https://` and ends with `.ts.net`. Copy it.
5. Tell Decurion this address. In Command Prompt, type `setx DECURION_PUBLIC_URL ` followed by the address, then press Enter. For example: `setx DECURION_PUBLIC_URL https://my-pc.example.ts.net`
6. Close Decurion and start it again, so it uses the new address.

On a Mac or Linux, set `DECURION_PUBLIC_URL` in the terminal before you run `decurion-start`, for example `export DECURION_PUBLIC_URL=https://my-pc.example.ts.net`.

## Pair your phone

1. On the board on your PC, open **Settings** and go to **Phones**.
2. Click **Pair a phone**. A square code (QR code) appears.
3. On your phone, open the camera and point it at the code. Tap the link that appears.
4. A page called **Pair this phone** opens. Tap **Pair this phone**.
5. Your phone shows the **Needs you** inbox.

The code works once, for 30 minutes. If you make a new code, the older unused one stops working.

## What you can do on your phone

Everything you can do on the board. The phone has four tabs at the bottom:

- **Inbox:** what needs you. Questions from agents show first, with answers you tap.
- **Board:** all your work, by state and by app. Tap **New work** to ask for something: send it to your project lead, or pick **Just do it** for a small change. Tap a card to accept it, send it back, stop it, move it between your PC and the cloud, talk to its agent, or take over its test browser.
- **Goals:** talk with your project lead, answer its questions, open the visual plan and approve it.
- **Settings:** apps (with **Choose folder** to pick an app's folder on your PC), websites, test keys, website sign-ins, your GitHub token, caps, usage, computers and phones. Keys and tokens you type are hidden, never shown again except their last four characters, and never kept on the phone.

## Turn on notifications

1. Add the inbox to your home screen:
   - **iPhone:** tap the Share button, then **Add to Home Screen**. Open Decurion from the new icon. On iPhone, notifications work only from the home screen icon.
   - **Android:** in Chrome, tap the menu, then **Add to Home screen**.
2. In the inbox, tap **Turn on notifications**.
3. When your phone asks, tap **Allow**.

Your phone now alerts you when work is ready for review, waiting on you, or stopped. It also alerts you when the pay-per-use spending cap is reached. Notifications show only a short line, never the agent's work.

## Remove a phone

1. In Settings, on the board or your phone, go to **Phones**.
2. Find the phone in the list and click **Remove**.
3. Click **OK** to confirm. That phone is signed out and needs a new code to come back.

## If something goes wrong

- **The board says "This address is not https."** Decurion does not know your Tailscale address. Do the Tailscale steps above, then restart Decurion.
- **The phone cannot open the link.** Check that Tailscale is on, on both the phone and the PC.
- **"This pairing code is used or expired."** Click **Pair a phone** on the board again and scan the new code.
- **"This phone is signed out."** The phone was removed from the board. Pair it again.
- **There is no Turn on notifications button.** You may have blocked notifications for this site before. Allow them in your phone's settings. On iPhone, open Decurion from the home screen icon.

Next: [Give your first goal](give-your-first-goal.md).
