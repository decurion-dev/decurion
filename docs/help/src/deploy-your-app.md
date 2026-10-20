# Deploy your app in one tap

This page shows how to put your app live after you merge, with one tap on your PC or your phone.

After you merge a pull request on GitHub, the new version is not live yet. With deploys turned on for an app, Decurion notices the merge and shows **Merged · ready to deploy** on that app's line. Tap **Deploy** once, and Decurion runs the app's own release on your PC and tells you when it is live. That one tap is the only ask: there is no "are you sure".

## Turn deploys on for an app

You do this once per app.

1. Open **Settings**, then **Apps**, then the app. Its **Rules** tab opens first. (From **Apps**, open the app and tap **Rules and settings**.)
2. Tap **Deploy from Decurion**. (**Full control** turns it on together with **Merge from Decurion**; **PR only** turns both off.)
3. In **Deploy steps**, write the commands that release the app, one per line, the same ones you'd type in the app's folder. For example:

   ```text
   npm ci
   npm ci --prefix infra
   node scripts/deploy-prod.mjs
   ```

4. In **Live site**, put where the app opens, such as `https://app.pvmedispa.com`. The **Open site** button uses it.
5. Tap **Save and turn on**.

Each line runs as one command. Decurion does not use a terminal to run them, so marks like `&&`, `|` or `>` are refused: put each command on its own line instead. At most 8 lines.

New apps start with deploys off. Turning **Deploy from Decurion** off again removes the button at once.

## Deploy after a merge

1. Merge the pull request on GitHub, as usual.
2. Within a couple of minutes, the app's line on **Home** says **Merged · ready to deploy**, and your phone gets a notice. The line moves to the top of **Needs you**, under **Apps that need you**. **What's in it** lists the changes since the last release. To look right away, tap **Check for changes**.
3. Tap **Deploy**.

The card shows how far it has got, such as "Step 3 of 3: node scripts/deploy-prod.mjs · 2/5 Snapshot". When it finishes, it says **Live** with an **Open site** button, and your phone gets a notice.

**Deploy steps** on the card opens the app's rules, where you can change them.

If you merge again before you tap, the same card moves to the newest version. You are never asked twice. **Not now** hides the card until the next merge.

## Merged is not live yet

For an app that deploys, a goal is not finished at the merge. Its last step is **Live**:

- On the goal's page, the Roadmap and Home, a merged goal says **Merged · ready to deploy** until a deploy carries that merge.
- The goal's **Deploy** button takes you to the app's line on Home, where you tap **Deploy**.
- Once the deploy finishes, the same goal says **Live**.

Apps that don't deploy from Decurion show **Done** at the merge, as before.

## What Decurion does when you tap

- It takes the exact version the card showed. If something newer was merged in the meantime, it says so and the card updates, so you never release something you were not shown.
- It makes a clean copy of that version on your PC, in Decurion's own folder. Your app's folder is not touched.
- It runs your deploy steps there, in order, with this PC's own sign-ins, such as the AWS command line's. Decurion's own keys are kept out of them.
- It stops at the first step that fails.

Only you can start a deploy. Agents, the project lead, voice, and the cloud worker cannot. Decurion never deploys by itself after a merge.

Decurion does not read your app's data. It keeps what the deploy steps print so you can see what happened, so make sure your deploy script never prints patient details.

## When the deploy steps change

Your deploy steps can run scripts from the app, such as `scripts/deploy-prod.mjs`. If one of those changed since the last release, the card says **Deploy steps changed** and names the file, and the button says **Deploy new steps**. Tapping it shows the steps first, so you can check them before they run.

Work by agents that changes one of those files is also held for you at review, like a protected part. See [Protected parts](improve-decurion-itself.md#protected-parts).

## When a deploy fails

The card says **Deploy failed** and which step stopped. Tap **Details** to see everything the steps printed, then **Try again** once you have fixed the cause. Common causes:

- **"Decurion could not find npm on this PC."** Install the program the step names, then tap **Try again**.
- **An AWS or other sign-in has run out.** Sign in again on this PC in a terminal, then tap **Try again**.
- **"Decurion stopped while this deploy was running."** The PC or Decurion restarted partway through, so part of the release may be live. Open the site to check, then tap **Try again**.

A deploy that runs longer than 90 minutes is stopped.

One app deploys at a time. If another app is deploying, wait for it to finish, then tap **Deploy** again.
