# Give your first goal

This page helps you give Decurion something to build.

A **goal** is what you want, in plain words. Your **project lead**, one agent
you talk to, turns it into finished, checked work.

## Start a goal

There is one box for asking for anything: **What do you want?**

1. Open the board at <http://decurion.localhost:7420>. The box is at the top.
   On your phone, open the inbox, tap **Board**, then **New work**. On the
   **Goals** page, press **New work** to open the same box above your goals.
2. Choose the app.
3. Type what you want, such as "Add a password reset page to the portal." Say
   what you want, not how to code it.
4. Leave **Project lead** chosen and press **Send to the project lead**.
   Decurion opens the goal on the **Goals** page.

### Project lead or Just do it

The box offers two ways to get it done:

- **Project lead** (the usual choice). The project lead asks what it needs,
  shows you a plan, and starts after you approve. A checker reviews each
  piece.
- **Just do it**. One agent does it right away. No plan and no checker. Best
  for small changes, such as "Make the footer text bigger." Only this choice
  shows the extra options: which AI does it, your PC or the cloud, a title,
  and whether it is part of other work. The new card shows on the board, under
  **Queued**, until an agent picks it up.

Nothing starts without you: a goal waits for your **Approve plan**, and
**Just do it** starts only when you press **Start work**.

## Answer a few questions

Your project lead reads the app's code first, then asks you a few questions.
Each question has choices, with its suggestion marked. Tap a choice, or write
your own answer. You can also type a message at any time.

## Look at the plan, then approve it

When it knows enough, your project lead shows a plan split into pieces of
work, each with how you will know it is done.

- Press **Open the visual plan** to see a page of pictures: how the app looks
  today beside how it will look after the change. For a new app, you see
  mockups. On your phone it opens inside the app; press **Back to the goal**
  when you are done.
- Want something different? Type it. You get a new plan.
- Happy with it? Press **Approve plan**, tap **Yes, do it**, or just type
  "yes" or "go ahead". All three do the same thing: they start the plan you
  see. That is the only approval the goal needs, and it works even while your
  project lead is still answering.
- Under the plan, **What happens next** says what Decurion does with each
  piece once it is checked. It follows your app's rules (see
  [Choose your rules](choose-your-rules.md)), so it may say a piece goes to
  GitHub by itself, or that each piece waits for your **Accept**.
- A piece marked **May change a protected part of the app** waits for your
  **Accept** at the end, even when the checker passes it. Protected parts are
  files the app lists as ones that always wait for you (see
  [Protected parts](improve-decurion-itself.md#protected-parts)). The plan
  says which pieces may change one. When none do, it says nothing about them.

## While the work runs

Each piece of work runs on its own and shows on the board. When a piece is
built, a checker agent tries it in a separate copy, takes "as built"
pictures, and either passes it or sends it back with notes. A piece gets at
most two checks; if it still has problems, it is marked **Needs you · take a look**. The
checker lists what it ran and marks each thing it says as **Checked** or
**Guessed**.

A check can take a while. While it runs, the checker counts under
**Running** on the board, with its own card, and the piece says "Checker
working" and when it started. A check lined up behind other work counts
under **Queued** and says "Waiting for a free agent to check it". If
something else holds it up, such as the Max plan being out of usage for now,
it says so and when it starts by itself. Until the check is done, the piece
says "Nothing to do. The checker looks at it first." and offers no
**Accept**; the buttons come back once the check has finished.

Pieces built later in a goal read short notes on what the earlier pieces did
and found, so you never have to pass them along yourself.

To find something out without changing the app, or to ask why a piece was
done a certain way, see [Research, Ask why, and the checker's proof](research-and-ask-why.md).

With the app's rule **Checked work goes to GitHub by itself** on (it is on
for new apps), a piece that passes goes to GitHub as a pull request by itself.
You approved the plan, so you are not asked again. Open the piece on the board
and its **Overview** says so: "It went to GitHub by itself because you
approved the plan", with a link back to the goal. With that rule off, each
piece waits for you to tap **Accept**. A piece that changes a protected part
always waits for your **Accept**, and its step bar says **Needs you · tap
Accept** and names the protected files.

## Follow each piece to live

The goal's page, and the panel of each of its pieces, show the same step bar:

**Built**, **Checked**, **On GitHub**, **Merged**, **Live**

A tick marks each step that is done and a dot marks the one it is on. Under the
bar, one line says what happens next and who does it, such as "It is on GitHub
as a pull request. You or your reviewer merge it there. Decurion sees the
merge by itself." Steps that do not apply are left out: **Live** shows only
for an app that deploys from Decurion (see
[Deploy your app in one tap](deploy-your-app.md)), or for Decurion itself
(see [Update Decurion](improve-decurion-itself.md#update-decurion)). Before
you approve the plan, every step is still to come.

A finished piece never just says Done. Its label says the step it is on, such
as **On GitHub** or **Merged**.

You can stop the goal at any time with **Stop goal**, under the **•••** button.

Your project lead keeps short notes about each app, such as how its tests run.
To see them, press **•••**, then **Notes on this app**. Remove any note that is
wrong or out of date.

## When it is ready

When every piece is finished, your project lead writes up what was done in
plain words and your phone gets a notice. Under the report you see each pull
request, numbered in the order to merge them, each with the step it is on.
Merge them on GitHub (or with **Merge**, if the app's rules allow it).

When every piece is merged, the goal closes by itself. For an app that deploys
from Decurion, and for Decurion itself, it closes once every piece is also
live. This works for pull requests you opened yourself from Decurion's
**Open pull request on GitHub** link too.

To end a goal sooner, press **Close this goal**. It moves the goal to your
finished goals. Nothing changes on GitHub or in your live app.
See [Review and merge work](review-and-merge.md).

## Tips for a good goal

- Say how you will know it works, such as "Users get an email with a link."
- One goal per change you would describe in one sentence.
- For a small change you can skip the plan: choose **Just do it** in the
  same box.

## If something goes wrong

- **"Your project lead got stuck."** Press **Try again**.
- **"This month's pay-per-use limit is used up."** Raise the cap in Settings,
  then press **Try again**.
- **A piece moves to Failed.** See [When something fails](when-something-fails.md).
