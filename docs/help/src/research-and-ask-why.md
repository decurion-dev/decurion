# Research, Ask why, and the checker's proof

This page helps you find things out without changing your app, ask why work
was done a certain way, and see how sure the checker is.

## Ask a question instead of asking for a change

Sometimes you want an answer, not new code. For example:

- "Find out how the old refill app stores schedules."
- "Is there any real patient data in the test database?"
- "Compare two ways to send reminders and tell me which is simpler."

Give it as a goal, the usual way (see [Give your first goal](give-your-first-goal.md)).
Your project lead plans a **research** piece for it. A plan can also mix
research pieces with pieces that build something.

A research piece:

- Changes no code. Its agent reads the app, and may run it or its tests, in its
  own throwaway copy.
- Writes a **report** page, in plain words. Each finding is marked
  **Checked** (the agent saw it for itself, and says how) or **Guessed** (the
  agent worked it out without seeing it).
- Skips the checker, because there is no code to check.
- Never goes to GitHub. Accepting it just marks it done.

When it finishes, open the goal's **Pieces of work** and press **Read the
report**. Under **What it found** you see the agent's short summary.

### Turn the findings into work

Under a finished research piece, press **Turn into a goal**. Decurion starts a
new goal with what the research found, and your project lead takes it from
there: it asks what it needs and shows you a plan. Nothing is built until you
approve that plan.

## Ask why

Under any piece whose agent has finished, press **Ask why**. Type your
question, such as "Why did it change the sign-in screen?", or leave it blank to
ask why it was done this way. Press **Ask**.

Your project lead reads what the agent really changed and the agent's own log,
then answers in the goal's conversation. It says what it saw and what it is
only guessing. Asking does not send the work back or start anything.

## The checker's proof

When the checker looks at a piece of work, it now lists:

- **What it ran**: the tests, the app, the screens it opened, and what came of
  each, such as "the app's tests: 42 of 42 passed".
- **What it found**: each thing it says about the work, marked **Checked** or
  **Guessed**.

You see these under **The checker says** on each piece, and they are in the
pull request on GitHub, under **The checker**. Treat a **Guessed** line as a
hint to look for yourself.

## If something goes wrong

- **A research piece moves to Failed.** Its agent finished without a report or
  a summary. See [When something fails](when-something-fails.md), then send it
  back to try again.
- **The research finished without a report.** The piece says **Needs you · take a look**.
  Open it and read its **Activity**, or send it back.
- **Ask why says the piece has not been built yet.** Wait until its agent has
  finished at least once.
