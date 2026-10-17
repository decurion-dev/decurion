# Choose your rules

This page helps you decide what agents may do on their own and what they must ask you about first.

> **Coming soon.** This part of Decurion is still being built. This page shows how it will work.

## How it will work

For anything an agent wants to do, your rules give one of three answers:

| Answer | Examples |
| --- | --- |
| Go ahead | Run the tests, save work in progress, change files in the app's own copy |
| Ask me first | Add new outside software, push code to GitHub, merge, change the database, deploy |
| Never | Delete things outside the app, read passwords or private keys |

During setup, you will pick one of three starting levels:

- **Careful** asks you most often.
- **Balanced** asks before adding software, pushing, merging, changing the database or deploying.
- **Fast** asks you least often.

You will be able to change any rule later. When a request reaches your inbox, you can tap **Always allow** so you are not asked again for that app.

## What is true today

You do not set rules yet. Instead, every agent works in a sealed workspace:

- It sees only its own copy of your app's code.
- It does not ask you for permission while it works.
- The only key it holds is the token for its Claude account. It has no GitHub key, so it cannot push code to GitHub.
- Your own app folder is never changed. You decide what to keep when you [review the work](review-and-merge.md).

Next: [Connect your phone](connect-your-phone.md).
