# Give an app its test keys

This page helps you give an app's agents the keys its tests need, such as a Stripe test key or a test database key.

A **test key** works only with a service's test or sandbox mode. It cannot charge real cards or touch real customer data. Decurion takes test keys only.

## Add a test key

1. Open the board and click **Settings**.
2. Open the **Test keys** tab and find the app the key is for.
3. In **Name**, type the name your app reads the key from, such as `STRIPE_SECRET_KEY`. Use capital letters, numbers and `_`.
4. In **Key**, paste the key. Click **Show** if you want to check it.
5. Click **Save key**.

The key now shows in the list as its name and its last four characters. Decurion never shows the whole key again. To change it, save it again under the same name. To stop agents getting it, click **Remove**.

## Good to know

- Keys are kept in Windows' own secure store on your PC, not in Decurion's files, your app's folder or GitHub.
- Each piece of work gets only its own app's keys, from its next run.
- If an agent writes a key into what it says, the board hides it.
- Before work goes to review, Decurion checks the agent's new commits for the keys. If it finds one, the work stops instead. Click **Retry**, and the agent is asked to take the key out of its commits. A key that was already in your app's code before the agent started does not count.

## If something goes wrong

- **"this looks like a live Stripe key"** (or an Anthropic, GitHub, AWS or Slack key, or a private key) Only test keys can go to agents. Use the service's test-mode key instead.
- **"… is used by Decurion or the agent's box; pick another name"** Names such as `PATH` or ones starting with `ANTHROPIC_`, `CLAUDE_`, `GIT_` or `DOCKER_` are taken. Pick another name and use that name in your app.
- **"a key cannot start or end with a space"** Copy the key again without the space.
- **Work fails with "test key … is missing from this PC's key store"** Save the key again on the board.
