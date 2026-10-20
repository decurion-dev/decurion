# Give agents extra tools with add-ons

This page helps you add an **add-on**: a small package someone wrote that gives your agents an extra tool, such as "look up tickets in our tracker" or "count the words in a page".

You add one by its link and approve it once. After that it never asks again, unless an update wants to do more than you agreed to.

## Add an add-on

1. Open Decurion on your PC and click **Settings** in the menu.
2. Open the **Add-ons** tab.
3. Paste the add-on's link, such as `https://github.com/example/words`, into **Add an add-on**. If the add-on is in a folder of that repo, open **It is in a folder of that repo** and type the folder.
4. Click **Check it**. Decurion fetches the add-on and shows one card. Nothing is added yet, and nothing from the add-on runs.
5. Read the card. It says, in plain words, what the add-on will be able to do: the tools it gives your agents, the websites it talks to, and any test key it uses. It also says what it can never do.
6. Under **Use it in**, tick the apps whose agents should get it. You can change this any time.
7. Click **Add**.

The add-on's tools reach those apps' agents from their next piece of work.

## Turn an add-on on or off for an app

1. In Settings, **Add-ons**, find the add-on.
2. Under **On for**, click the switch next to the app.

It applies from the app's next piece of work. If the switch next to an app is greyed out, the add-on needs a test key that app does not have yet. Add it in **Test keys** (see [Give an app its test keys](add-test-keys.md)), then come back.

## Update an add-on

1. In Settings, **Add-ons**, click **Check for an update** next to the add-on.
2. If the new version asks for nothing more than you already agreed to, it is updated straight away and Decurion says so.
3. If it asks for more (a new tool, a new website, a key, or a tool that can now change things), you see a card listing only what is new. Click **Update** to agree, or **Cancel** to keep the version you have. The old version keeps working until you decide.

## Remove an add-on

1. In Settings, **Add-ons**, click **Remove** next to the add-on.
2. Click **Remove add-on** to confirm.

New work stops getting its tools at once. Work already running finishes with it.

## Good to know

- Each tool runs in a sealed box of its own beside your agent. It gets none of your app's code and never sees your Claude sign-in.
- It reaches only the websites your agents may use, plus the ones it listed on its card, and only while it works for an app it is on for. Your agents can reach those sites too.
- If an add-on uses a test key, your agents can use that key too, like every test key, and Decurion hides it from what they report. Use test keys only.
- Decurion keeps the exact version you approved. If even one byte of it changes, Decurion does not use it, and the work's activity says so.
- The rules that always stop for you still do. No add-on can merge to main, deploy, touch production data, spend past your cap or send messages to people. Add-ons that would do those are refused when you click **Check it**.
- **What add-ons did lately**, at the bottom of the tab, lists every add, update, removal, switch, and each piece of work that used an add-on or its key.
- For now, add-ons give tools only to agents that use Claude, and only for pieces of work (not to the project lead or the checker).

## If something goes wrong

- **"Decurion could not fetch the add-on from that link"** Check the link opens in your browser. Decurion fetches add-ons without signing in, so the repo must be public.
- **"This add-on adds a new kind of agent"** or **"an automatic check"** This version of Decurion only adds tools. Look for a version of the add-on that is only a tool.
- **"… must wait for your OK every time, which Decurion cannot do for add-on tools yet"** The add-on sends messages, deploys, touches live data, spends money, or talks to a website that does. Decurion refuses it to keep those stops in your hands.
- **"… has no test key named …"** Add that test key to the app first, or leave the app unticked.
- **The work's activity says an add-on "was left out of this run"** Its files could not be fetched or had changed. Remove the add-on and add it again from its link, then check the card.
