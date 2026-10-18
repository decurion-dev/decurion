# Save website sign-ins

This page helps you let an app's agents check pages that need a sign-in, such as your app's admin pages on a staging site.

You sign in once, yourself, in a browser window that runs on your PC and shows on your screen or phone. Decurion saves that sign-in (what a browser keeps to stay signed in), not your password. From then on, the app's agents start their test browser already signed in.

## Use test accounts only

Sign in only with a **test account**: one made for testing, on a test or staging site, that can't reach real customers, real money or your own email.

Why: most of what keeps a browser signed in is locked away from the agent, but some websites also keep a sign-in token where any page can read it. An agent could read such a token and copy it somewhere. Decurion hides these values from what agents write and refuses commits that contain them, but it can't promise an agent never sends one out. With a test account, nothing real is at risk.

## Add a website sign-in

1. Open the board and click **Settings**.
2. Open the **Website sign-ins** tab and find the app the sign-in is for.
3. In **Name**, type your own name for it, such as `Staging admin`.
4. In **Sign-in page**, paste the address of the website's sign-in page. It must start with `https://`. Sites running on your own PC (`localhost`) can't be signed in to this way yet. Don't put a name or password in the address.
5. If you like, in **Page to check**, paste the address of a page that only works when signed in, such as your account page. Before each run, Decurion opens it to check the sign-in still works.
6. Click **Add sign-in**.
7. Click **Sign in** next to it. A browser window opens at the sign-in page.
8. Sign in with the test account, as you normally would: click a field in the picture and type straight into it. On a phone, tap the field, then tap **Keyboard**.
9. When you see that you are signed in, tap **I'm signed in, save**.

The list now shows **Signed in**, until when, and which websites the sign-in covers. Your password was typed into the website only; Decurion never keeps it.

## When a sign-in stops working

Sign-ins run out, like they do in your own browser. Decurion tells you:

- A day before a sign-in runs out, the inbox shows **Sign in to … again**.
- When it has run out, or an agent's browser finds the website signed out, the inbox shows the same card and your phone gets a notification.

Tap the card, or **Sign in again** on the board, and sign in once more. Agents get the new sign-in from their next run.

## Good to know

- Saved sign-ins stay on your PC, locked with a key kept in Windows' own secure store. They are never in Decurion's database, your app's folder or GitHub.
- Each piece of work gets only its own app's sign-ins.
- Agents can only visit the websites on the app's list in **Settings**. Add the website the sign-in is for there too, or agents will be turned away from it.
- Saved sign-ins never leave your PC. Work running on a cloud worker goes without them.
- An agent's own changes to a sign-in (signing out, say) are never saved. The next run starts from your saved sign-in again.
- To stop agents being signed in, click **Remove**. The saved sign-in is deleted from your PC.

## If something goes wrong

- **"the address must start with https://"** Use the website's https address.
- **"leave the name and password out of the address"** Remove the `name:password@` part. You type those into the sign-in window instead.
- **The sign-in window says it is not available yet** Your hub is older than this feature. Update Decurion and try again.
- **The card keeps coming back right after you sign in** The website may sign people out quickly, or the page to check may need more than a sign-in. Try another page to check, or remove it.
