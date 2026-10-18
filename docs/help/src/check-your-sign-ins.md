# Check your sign-ins

This page helps you see, in one place, whether everything Decurion signs in with still works, and fix what doesn't.

## Open the Sign-ins list

1. Open the board, or the Settings tab on your phone.
2. Click **Settings**. **Sign-ins** is the first section.

Each row is one account, with a light:

- **Green:** fine.
- **Amber:** needs you soon, for example it runs out within a week.
- **Red:** needs you now. Work that uses it can't start.
- **Grey ring:** not set up. Nothing needs it yet.

Where Decurion knows when a sign-in runs out, the row says so, such as "Runs out in 5 days" or "Good until Oct 18, 2027".

## What the list shows

- **Claude A and Claude B.** Your two Max plans. A Claude sign-in lasts one year. Decurion knows the date when you signed in with **Sign in on this PC**. For one saved the older way, it shows **Signed in** without a date until you sign in here once.
- **GitHub.** The token that opens pull requests for you. GitHub tells Decurion when a token runs out, and whether it still works.
- **Phone connection.** Shown only when phones can't reach Decurion yet. Click **Connect your phone**. See [Connect your phone](connect-your-phone.md).
- **Each phone and browser.** It stays signed in while you use it. One you don't use for a year is signed out.
- **Cloud worker.** Whether it is online. See [Add the cloud worker](add-the-cloud-worker.md).
- **Each website sign-in.** See [Save website sign-ins](save-website-sign-ins.md).

## Sign in to Claude on this PC

1. In **Sign-ins**, click **Sign in on this PC** next to Claude A (or Claude B).
2. Claude's own sign-in page opens in your PC's browser. Sign in with the Claude account for that plan, and allow access.
3. Come back to Decurion. After a moment it says **Signed in**, and the light turns green.

You can also tap the button on your phone. The page still opens on your PC, so do this when you are at your PC.

Decurion never sees your Claude password. It keeps only the sign-in Claude gives back, in Windows' secure store, and never shows it or writes it to a log.

If the page did not open, click **The page didn't open?**, then **Open Claude's sign-in page**. Sign in, copy the code Claude's page shows, paste it into **Code from Claude's page** and click **Send code**.

To change which account a plan uses, click **Sign in again** next to it.

## Reminders

A week before something with a known date runs out, your phone gets a notification such as "Claude A runs out in 7 days". Tap it to open Sign-ins. Each sign-in is reminded once for each date. Renew it and the light turns green again.

## If something goes wrong

- **"Claude Code is not installed on this PC."** Install Claude Code (the installer offers it), then click the button again.
- **"A Claude sign-in is already open."** Finish it in the browser, or click **Cancel** at the top of Sign-ins, then start again.
- **"Claude Code didn't start its sign-in on this PC."** Decurion waits up to 45 seconds for Claude's page. Update Claude Code, then click **Try again**. If the message ends with what Claude said, that line tells whoever helps you what went wrong.
- **"The sign-in took too long."** Decurion waits 10 minutes. Click **Try again**.
- **Claude B signs in as your Claude A account.** Your browser is still signed in to Claude A. Sign out of Claude in that browser, then click **Sign in on this PC** next to Claude B again.
- **GitHub is red: "GitHub no longer takes this token."** Make a new token on GitHub and save it in Settings, GitHub. See [Review and merge work](review-and-merge.md).
