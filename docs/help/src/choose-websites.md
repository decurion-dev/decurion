# Choose which websites agents may use

This page helps you let an app's agents reach a website they need, such as a payment service's test site.

Agents work in a closed room with one door to the internet, called the **website gate**. Through it they can reach only:

- The AIs: Claude, ChatGPT and Grok.
- The package stores where code libraries come from: npm, PyPI and crates.io.
- The websites you add for that app.

They can never reach your PC, your home network or your other devices.

## Add a website

1. Open **Settings**, then **Apps**, then the app.
2. Open the **Websites** tab.
3. Type the site, such as `stripe.com`, and click **Add**. An entry like `stripe.com` also covers addresses under it, such as `api.stripe.com`. You can add several at once, with spaces or commas between them.

To see the sites that are always allowed, open **See the websites always allowed** under the list.

## Remove a website

1. On the same **Websites** tab, find the site.
2. Click the **×** next to it.

Changes apply to work that starts or resumes after you save.

## When an agent tries another website

If an agent tries a website that is not on the list, the visit is turned away and the agent carries on without it. You are not asked anything. The work's activity shows **Blocked a visit to** that site. If the work needs the site, add it and send the work back.

A website on the list can receive information as well as send it, so add only sites you trust.

## If something goes wrong

- **"Type a website name, like stripe.com."** The box was empty. Type a site's name.
- **"stripe.com is already on the list."** Nothing to do. The site is allowed already.

Next: [Give an app its test keys](add-test-keys.md).
