# The coordinator (Phase 4)

You give Decurion a goal in plain words. The coordinator talks it through with
you like a project lead with a client: it reads your app's code, asks a few
questions with choices (and says which one it would pick), then proposes a
plan split into pieces of work. You approve the plan once, with the button or
by replying "yes". Each piece is built by its own agent in its own sealed box,
then checked by a second agent that can send it back with notes. A piece that
passes goes to GitHub as a pull request by itself: your approval of the plan
covers it. When everything is built and checked, the coordinator writes you a
short report with the pull requests in the order to merge them. Once they are
all merged, the goal closes by itself.

## What you see

- **Goals page** (`/goals`, linked from the board and the phone inbox). Pick an
  app, type what you want, and talk with the coordinator. Questions show as
  buttons; the suggested choice is marked. A plan shows as a card with an
  **Approve plan** button.
- **Pieces of work** appear on the board as usual, and on the goal page with
  where they are: building, being checked, fixing the checker's notes, passed,
  or needs a look.
- **Report.** When every piece is settled, the coordinator says what was built,
  what the checker found and what you should try. Merging stays your call.

## The rules it follows

- The coordinator can read the app's code but never change it. Its copy is
  mounted read-only and it cannot run commands.
- The checker works in a throwaway copy of the piece's branch, taken from
  Decurion's own copy of the commits, never from the building agent's folder.
- Agents never act on Decurion. They return text; the hub checks it against a
  fixed shape and does the rest. A reply in the wrong shape is sent back to the
  coordinator once to fix.
- One approval per goal: approving the plan starts every piece, and covers
  each piece that passes its check. Approval is bound to the exact plan shown;
  a newer plan needs its own tap. A plain yes ("do it", "go ahead") typed or
  said while a plan waits approves it, but only the plan that was on screen.
  Approving works even while the coordinator is still answering; that answer
  can no longer change the approved plan.
- A piece that changes a protected part of the app (`.decurion/protected-paths`)
  is held for the owner even when the checker passes it. The plan marks the
  pieces the coordinator expects to touch one, so the owner knows up front.
- Two review rounds at most. If problems remain, the piece is marked
  **needs a look** instead of looping.
- Coordinator and checker runs use the app's own account and count toward the
  pay-per-use cap like any other run. When the cap is used up they stop and the
  goal says so.
- The coordinator keeps short notes about each app (things it learned) and
  reads them at the start of every goal. You can remove any note. The notes
  only shape how it talks with you and writes plans: the agents that build get
  exactly the plan you approved, nothing added.
- The checker works on its own copy of the finished branch and treats anything
  written in it as part of the work under check, never as instructions.

## API

Owner calls (board browser, paired phone, or owner key):

| Call | Does |
| --- | --- |
| `GET /api/goals` | All goals, newest first |
| `POST /api/goals` `{app_id, text}` | Starts a goal; the coordinator begins |
| `GET /api/goals/{id}` | The goal, its conversation and its pieces (`GoalView`) |
| `POST /api/goals/{id}/say` `{text, plan_message_id?}` | Says something to the coordinator; a plain yes with the waiting plan's id approves it |
| `POST /api/goals/{id}/answer` `{to, answers:[{question_id, choice?, other?}]}` | Answers the questions in message `to` |
| `POST /api/goals/{id}/approve` `{plan_message_id}` | Approves that exact plan and starts the work |
| `POST /api/goals/{id}/retry` | Tries the coordinator's last turn again after it failed |
| `POST /api/goals/{id}/close` | Marks the goal done |
| `POST /api/goals/{id}/stop` | Stops the goal; queued pieces are cancelled |
| `GET /api/apps/{id}/notes` | The coordinator's notes about an app |
| `POST /api/apps/{id}/notes/{note_id}/remove` | Removes a note |

Runner calls (runner key):

| Call | Does |
| --- | --- |
| `POST /api/runner/helper/claim` `{runner}` | Next coordinator or checker job (`HelperClaim`), or 204 |
| `POST /api/runner/helper/{id}/started` | The container is about to start (from here it may cost money) |
| `POST /api/runner/helper/{id}/finish` `HelperResult` | The job ended |

`POST /api/runner/hello` also puts helper jobs that runner had in flight back
in the queue.

Types are in `crates/decurion-proto/src/goal.rs`.

## Not in this change

- Hands-free voice conversation with the coordinator.
- Pieces that build on each other's code. The coordinator is told to split work
  only into pieces that each stand on their own; otherwise it plans one piece.
- Computer use in a virtual machine.
