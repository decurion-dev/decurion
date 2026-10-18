# Talk to an agent while it works

This page shows how to change an agent's direction, give it more to do, or tell it something, without stopping its work.

## Send a message

1. On the board, click the card for the work.
2. Find **Tell the agent something** in the panel. On your phone, tap **Tell the agent something** on the card.
3. Type your message.
4. Click the button for what you want:

| Choice | What the agent does | Use it when |
| --- | --- | --- |
| **Change course now** | Stops the step it is on and does what you wrote instead. | The agent is going the wrong way. |
| **Do this next** | Finishes what it is doing, then does what you wrote. | You thought of more work for the same task. |
| **Add a note** | Keeps going and uses your note if it helps. It does not reply. | You want to share a fact, like "the test server is down today". |

Your message shows in the activity log as soon as Decurion has it. Until the agent reads it, the card shows **Waiting to reach the agent**.

You can also send a message to work that is waiting for you, in review, or stopped. **Change course now** and **Do this next** send that work back to the agent with your message. You cannot send messages to work that is done.

## Split work into smaller tasks

A big task can have smaller tasks inside it.

1. Open **What do you want?**: at the top of the board, or **New work** on your phone's Board tab.
2. Write the task and choose **Just do it**.
3. Under **Part of other work**, pick the bigger task this one belongs to.
4. Click **Start work**.

The bigger task's card lists its smaller tasks.

When you click **Stop** on the bigger task, Decurion stops its smaller tasks first, then the bigger task. To let a smaller task keep going when its bigger task stops, check **Keep going if the parent stops** when you add it.

## When work was cut off

If your PC restarts or Decurion stops while an agent is in the middle of a step, the log shows **Interrupted while running**, followed by the step's name.

- If the step only looked at things, such as reading a file, the agent runs it again.
- If the step changed something, such as running a command or editing a file, the agent first checks what actually happened. It never simply repeats the step, so nothing is done twice.

## Tapping twice is safe

If your connection drops while you tap a button or send a message, Decurion tries once more by itself. Each tap carries a receipt number, so the same tap never counts twice.

If the work changed while your screen was open, for example it moved to review before you tapped **Stop**, Decurion ignores the old tap and the card shows what is true now.
