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

The first time you run an app, Decurion has to fetch what the app needs before it can start, and that can take a few minutes. The picture shows how far it has got: installing what the app needs, then starting the app. After that, starting is much quicker.

## Use the app

| To do this | Do this |
| --- | --- |
| Click something | Tap or click it in the picture. |
| Scroll | Drag the picture up or down. On a PC, you can also use the mouse wheel. |
| Type | On a PC, click the field in the picture and type. On a phone, tap the field, then tap **Keyboard** under the picture and type. Pasting works too. While a ring shows around the picture, your typing goes to the app. |
| Press a key | On a PC, use your keyboard: Enter, Tab, Backspace, Delete, the arrows, and Ctrl+A (select all), Ctrl+Z (undo) and Ctrl+Y (redo). On a Mac, use Cmd. On a phone, tap **Enter**, **Tab** or **⌫** under the picture. |
| Go back a page | Tap **Back**. On a PC it sits beside the address. |

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

Instead of the picture, you see **Your app isn't running** and the reason. If the app printed anything before it stopped, **What it printed** shows the last lines.

Most often, Decurion doesn't know how to start this app yet. It can guess for most simple apps. But it can't guess for an app made of several parts that each start, such as a web app with a separate server and a database, because starting only one part would look broken.

When the app itself is the reason, tap **Ask an agent to fix it**. Decurion starts a piece of work that writes a start recipe: a small file in the app called `.decurion/run.json`. It says what to install, how to start every part the app needs, and which port to open. The agent then starts the app the same way and checks the main page works. A recipe looks like this:

```json
{"install": "npm ci", "start": "npm run dev", "port": 5173}
```

To try the recipe, tap **Open that work**. When the work is finished, tap **Run it** under **Try it**. The **Run it** button in **Today** runs your app's folder on this PC, so it uses the recipe once that folder has the merged change.

If the reason is about Docker or your PC, make sure Docker Desktop is running, then tap **Run it again**.
