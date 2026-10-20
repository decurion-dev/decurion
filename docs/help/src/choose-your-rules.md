# Choose your rules

This page helps you decide what Decurion may do for each app: only open pull requests, or also merge and deploy.

Each app has its own rules. Only you can change them. Agents never can.

## Pick PR only or Full control

1. Open **Settings**, then **Apps**, then the app. Its **Rules** tab opens first.
2. Tap one of the two buttons at the top:
   - **PR only**: Decurion sends the work to GitHub and opens the pull request. Nothing more. You or your reviewer merge on GitHub. **New apps always start at PR only.**
   - **Full control**: Decurion may also merge and deploy the app, one tap each. Finished pull requests get a **Merge** button, and merged work gets a **Deploy** button.

When you tap **Full control** and no deploy steps are saved yet, Merge turns on and the **Deploy steps** box opens, asking for them. See [Deploy your app in one tap](deploy-your-app.md).

## Set each rule on its own

Below the two buttons, each rule has its own switch. If you mix them, the app shows **Your own mix**.

- **Merge from Decurion.** Finished pull requests get a **Merge** button on Home, on your PC and your phone. One tap merges, and only once GitHub says the checks passed and nothing conflicts. Nothing merges by itself. See [Merge from Decurion](review-and-merge.md#merge-from-decurion).
- **Deploy from Decurion.** After a merge you get a **Ready to deploy** card. One tap runs the app's deploy steps on your PC. Decurion can't turn this on until you save the steps:
  1. In **Deploy steps**, type the commands that deploy the app, one per line, the same ones you'd type in the app's folder. Up to 8. Each line runs as one program, without a shell (the program that reads typed commands), so marks like `|`, `&&` or `>` are refused. Put each command on its own line instead.
  2. In **Live site**, type the address of the live app, such as `https://app.example.com`. This is optional. **Open site** goes there when a deploy finishes.
  3. Tap **Save and turn on** (or **Save** when Deploy is already on).
- **Checked work goes to GitHub by itself.** On for new apps. A piece of a goal you approved goes up as a pull request once the checker passes it. Turn it off to have each piece wait for your **Accept** instead.
- **Fix conflicts with main by itself.** Off for new apps. When one of its pull requests conflicts with main, Decurion starts **Bring up to date** by itself, once for each version of the pull request it sends, and the result still waits for your **Accept**. See [Fix conflicts with main by itself](review-and-merge.md#fix-conflicts-with-main-by-itself).
- **Its live version holds patient data.** On for new apps. Agents are told to use made-up data only, never real records. Decurion never sees patient data either way.

## Choose who reviews

You can name one person to review each pull request for an app, such as a teammate.

1. On the app's **Rules** tab, in **Who reviews its pull requests**, type their GitHub name, such as `param-k`.
2. Click **Save**.

They are asked to review each pull request for this app. To stop asking anyone, empty the box and click **Save**. See [Someone else reviews](review-and-merge.md#someone-else-reviews-decurion-keeps-watch).

## What is always on

At the bottom of the **Rules** tab, **Always on, for every app** lists the safety walls you can't switch off:

- Each piece of work runs in its own sealed box.
- Agents never hold your GitHub, AWS or deploy sign-ins.
- Agents reach only Claude, the package stores and this app's websites.
- Nothing merges or deploys without your tap, and agents can never do either.
- Nothing is ever pushed straight to your main branch.
- Pay-per-use spending stops at your caps.

Inside its sealed box, an agent works without asking you. It sees only its own copy of your app's code. Your own app folder is never changed.

> **Not built yet.** Decurion does not ask you before single steps inside the box, such as installing a code library. There are no **Approve**, **Deny** or **Always allow** buttons for those steps yet. The sealed box and the rules above are what keep your PC safe today.

## If something goes wrong

- **"Type their GitHub name, like param-k. Only letters, digits and single dashes."** Use their GitHub name only, with no spaces.
- **"Step on line 2 uses |, which Decurion does not run"** Each deploy step runs as one program. Split it into separate lines, one command each.
- **"Add the deploy steps first"** Deploy from Decurion needs saved steps. Type them in **Deploy steps** and tap **Save and turn on**.
- **"The live site must start with https://, like https://app.example.com."** Type the full address, starting with `https://`.

Next: [Choose which websites agents may use](choose-websites.md).
