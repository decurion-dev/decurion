# Run your app and try it

This page shows how to start your app on your PC and try it yourself, on the board or your phone, without opening a terminal.

**Run it** starts your app on your PC in a sealed box, opens it in a browser beside it, and shows you a live picture of that browser. You can click, scroll and type in it, as if the app were open in front of you. Nothing you do in it is saved into the code.

## Run one piece of work

Use this to try what an agent made before you accept it.

1. On the board, click the card for the work. On the phone, find it on the **Board** tab.
2. Under **Try it**, tap **Run it**.

This runs the work's last saved version: what the agent had made at the end of its last run. Above the picture, **Try it** says so: "This piece of work, as the agent last saved it." **Try it** shows for work that isn't running. While an agent is still working, the app keeps changing, so watch its test browser instead (see below).

Research has no **Try it**: it changes no code, so there is nothing new to run.

## Run the whole app

Use this to try your app as it is now, before any new work.

- On the board, each app in the **Today** strip has a **Run it** button. When the board shows only one app, the button is also next to the title.
- On the phone, each app in the **Today** card at the top of the inbox has the same button. On a small phone it shows only a play sign.

This runs the app as it is in its folder on your PC: the main version, without work that hasn't been merged yet. Under the app's name, the panel says **Your PC's copy**, so you know which version you are looking at. Work you merged on GitHub is in it only once that folder has been brought up to date. To try a newer piece of work, open that work and tap **Run it** under **Try it**.

While an app is running, its button says **Open**. Tap it to see the app again.

## The first time is slow

The first time you run an app, Decurion has to fetch what the app needs before it can start, and that can take a few minutes. The picture shows how far it has got: getting the code, installing what the app needs, and starting the app. After that, starting is much quicker.

## Use the app

| To do this | Do this |
| --- | --- |
| Click something | Tap or click it in the picture. |
| Scroll | Drag the picture up or down. On a PC, you can also use the mouse wheel. |
| Type | Tap the field in the picture first, then type in the box under the picture and tap **Send**. On a PC, you can also click the picture and type straight away. |
| Press a key | Tap **Enter**, **Tab** or **Backspace**. |
| Go back a page | Tap **Back**. |

On a PC, click **Bigger** to fill the window. Click **Smaller** to go back.

The address above the picture comes from the browser itself, so a page cannot pretend to be somewhere else.

## Stop it

Tap **Stop** under the picture when you are done. You don't have to: the app stops by itself after 15 minutes with nobody looking at it.

Closing the picture does not stop the app straight away. You can open it again from the same button within those 15 minutes.

Only one app runs at a time. Starting another app, or another piece of work, stops the one before.

## Good to know

- The app gets its own test keys, the ones in **Settings**, **Test keys**, so the parts that need a key can work. See [Give an app its test keys](add-test-keys.md).
- The app can reach only the websites on its list, the same as agents. If part of the app stays blank or says it can't connect, add the website it loads from to the app's list in **Settings**.
- Saved website sign-ins are not used. If the app needs you signed in somewhere, sign in inside the picture with a test account.
- Don't type real passwords or real customer details into it.
- Nothing you do in it is saved into the code. If you want something changed, tell the agent: send the work back with a message, or give a new goal.

## Agents run it too

Agents already start your app themselves to check their work. To see what an agent sees while it works, open its running card and tap **Watch the browser**. See [Watch and take over the test browser](take-over-the-browser.md).

## If it doesn't start

The picture says why. Most often, Decurion doesn't know how to start this app yet. Ask an agent for a start recipe, for example as **Just do it** work:

> Add a Run it recipe so Decurion can start this app

The agent writes a small file in the app called `.decurion/run.json`. It looks like this, and says what to install, how to start the app, and which port the app opens on:

```json
{"install": "npm ci", "start": "npm run dev", "port": 5173}
```

To try the recipe, open that piece of work when it's finished and tap **Run it** under **Try it**. The **Run it** button in **Today** runs your app's folder on this PC, so it uses the recipe once that folder has the merged change.
