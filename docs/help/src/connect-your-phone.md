# Connect your phone

This page helps you get Decurion's inbox on your phone, with notifications.

Your phone reaches your PC through **Tailscale**, a free app that makes a private network between your own devices. Nobody else can reach Decurion this way.

## Set up Tailscale (once)

1. Install Tailscale on your PC and on your phone. Get it from [tailscale.com/download](https://tailscale.com/download).
2. Sign in to the same Tailscale account on both.

That is all you do outside Decurion. Decurion sets up the rest itself.

## Pair your phone

1. On the board on your PC, open **Settings** and go to **Phones**.
2. Click **Connect your phone**. Decurion asks Tailscale for your PC's private `https://` address and remembers it.
3. The first time, Decurion may say **Tailscale needs HTTPS turned on once**. Open the link it shows, turn HTTPS on, then click **Connect your phone** again.
4. A square code (QR code) appears.
5. On your phone, open the camera and point it at the code. Tap the link that appears.
6. A page called **Pair this phone** opens. Tap **Pair this phone**.
7. Your phone shows the **Needs you** inbox.

The code works once, for 30 minutes. If you make a new code, the older unused one stops working.

Once Decurion has the address, the button reads **Pair a phone**. Click it to pair another phone. You never need to connect again, even after a restart.

On a Mac or Linux, `decurion-start` works the same way. On Linux, run `sudo tailscale set --operator=$USER` once first, so Decurion may set up Tailscale. If you set `DECURION_PUBLIC_URL` yourself, Decurion uses that address instead.

## What you can do on your phone

Everything you can do on the board. The phone has four tabs at the bottom:

- **Inbox:** what needs you. Questions from agents show first, with answers you tap. Plans waiting for your approval and your project lead's questions show here too, with the app's name, **Approve** and **Open plan**.
- **Board:** all your work, by state and by app. Tap **New work** to ask for something: send it to your project lead, or pick **Just do it** for a small change. Tap a card to accept it, send it back, stop it, move it between your PC and the cloud, talk to its agent, or take over its test browser.
- **Goals:** talk with your project lead, answer its questions, open the visual plan and approve it.
- **Settings:** apps (with **Choose folder** to pick an app's folder on your PC), websites, test keys, website sign-ins, your GitHub token, caps, usage, computers and phones. Keys and tokens you type are hidden, never shown again except their last four characters, and never kept on the phone.

## Turn on notifications

1. Add the inbox to your home screen:
   - **iPhone:** tap the Share button, then **Add to Home Screen**. Open Decurion from the new icon. On iPhone, notifications work only from the home screen icon.
   - **Android:** in Chrome, tap the menu, then **Add to Home screen**.
2. In the inbox, tap **Turn on notifications**.
3. When your phone asks, tap **Allow**.

Your phone now alerts you when a plan is ready, your project lead has a question, work is ready for review, waiting on you, or stopped, and when work is ready to merge on GitHub. It also alerts you when the pay-per-use spending cap is reached. Each notice starts with the app's name, like "Refill tracker: ready for review". Notifications show only a short line, never the agent's work.

### Act right from the notice

On Android, and on a PC, some notices have buttons, so a plain yes needs no trip into the app:

- **Approve plan** approves the plan the notice is about. If your project lead has written a newer plan since, nothing is approved and the notice tells you to open the plan.
- **Accept** accepts finished work, and Decurion sends it to GitHub. **Look first** opens the inbox instead.
- **Open pull request** opens the work on GitHub, where you merge it.

The notice then changes to say what happened, such as "Refill tracker: plan approved". A button does the same as the button in the app, and tapping it on two phones still does it only once. Only a phone you paired can use these buttons. iPhones don't show buttons on notices; tap the notice to open the inbox instead.

## Remove a phone

1. In Settings, on the board or your phone, go to **Phones**.
2. Find the phone in the list and click **Remove**.
3. Click **OK** to confirm. That phone is signed out and needs a new code to come back.

## If something goes wrong

- **"Tailscale is not on this PC."** Install Tailscale from the link Decurion shows, sign in, then click **Connect your phone** again.
- **"Tailscale is not signed in on this PC."** Open Tailscale on your PC and sign in. Then click **Connect your phone** again.
- **"Tailscale needs HTTPS turned on once."** Open the link Decurion shows, turn HTTPS on, then click **Connect your phone** again.
- **"Tailscale has no name for this PC yet."** Open the link Decurion shows and turn on MagicDNS. Then click **Connect your phone** again.
- **"Tailscale took too long to answer."** Check that Tailscale is on, on your PC. Then try again.
- **The board says "This address is not https."** Decurion does not know your Tailscale address yet. Click **Connect your phone**.
- **The phone cannot open the link.** Check that Tailscale is on, on both the phone and the PC.
- **"This pairing code is used or expired."** Click **Pair a phone** on the board again and scan the new code.
- **"This phone is signed out."** The phone was removed from the board. Pair it again.
- **There is no Turn on notifications button.** You may have blocked notifications for this site before. Allow them in your phone's settings. On iPhone, open Decurion from the home screen icon.

Next: [Give your first goal](give-your-first-goal.md).
