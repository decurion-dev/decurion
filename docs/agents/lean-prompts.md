# Lean prompts, the prompt cache and compaction

What a Claude agent in a Decurion box reads before its first turn, what was
trimmed and why, how compaction is set, and how tokens are counted. Read this
before you change `run_args`, `helper_run_args`, `box_args` or the token code.

## What an agent starts with

Measured on Claude Code 2.1.289 (the pinned version), with Decurion's own repo
as the app (its `CLAUDE.md` and `AGENTS.md` are about 3,700 tokens) and the
browser MCP server attached. Tokens are characters divided by 4: close enough
to compare, not exact.

| Part | Before | After |
| --- | --- | --- |
| System prompt | 1,481 | 850 |
| Tools | 46 tools, 17,405 (browser 25 = 4,696) | 35 tools, 9,696 |
| First messages (CLAUDE.md, git status, skills list, environment) | 6,449 | 4,822 |
| **Total before the task** | **about 25,300** | **about 15,400** |

When Claude Code defers MCP tools until they are needed (tool search, which it
turns on by itself when it talks to Anthropic directly; inferred from the CLI,
not seen on a live account), the same start is about 15,000 before and 8,900
after.

What was cut:

- **Skills** (`--disable-slash-commands`): Claude Code's own skills are for a
  person at a terminal (loop, schedule, keybindings, plugin authoring and so
  on), and their list rode along on every turn (about 2,200 tokens). A repo's
  own skills now never load either, matching wall 2.
- **Tools a sealed box cannot use** (`UNUSED_TOOLS`, about 7,000 tokens):
  timers and wake-ups, git worktrees, remote triggers, agent lists, multi-agent
  workflows, review reports. Kept: Agent (subagents), Bash, Read, Edit, Write,
  NotebookEdit, SendMessage, TaskStop, WebFetch, WebSearch and the browser.
- **Auto memory** (`CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`): its instructions are
  about 600 tokens a turn, and the project lead keeps each app's notes.
- **Per-box text in the system prompt**
  (`--exclude-dynamic-system-prompt-sections`): the folder, git status and
  memory path move into the first message, so the system prompt and tool list
  are the same in every box. Claude's prompt cache is matched from the start
  of the request, so a new piece on the same Max plan can read that part back
  instead of afresh.

The app's own `CLAUDE.md` and `AGENTS.md` still load: they are the most useful
thing an agent reads. Keep the tool list and flags the same on every start of
a session; a changed tool list makes Claude read the whole conversation afresh.

## The prompt cache

On a Max plan, Claude Code asks for a one-hour cache for the main
conversation and five minutes for subagents (Claude Code docs, prompt
caching). Pay-per-use runs get five minutes. Cache reads cost about a tenth of
fresh input. Nothing Decurion does changes the order Claude Code builds a
request in (tools, system prompt, then messages), which is what makes the
cache work; do not add text that changes per run to the system prompt.

## Compaction

Findings, from the stand-in API below reporting a conversation of a chosen
size:

- With nothing set, a 1M-token model (Opus 5.5 on a Max plan) compacts at
  about 970k-980k tokens, during a turn or between turns.
- `--autocompact 200000` (or `CLAUDE_CODE_AUTO_COMPACT_WINDOW`, or
  `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`) makes it compact when the next turn
  starts once the conversation is past that size: an owner message, a resumed
  run, a fix round after a failed check. Inside one turn's run of tool calls,
  2.1.289 still goes to about 970k.
- `CLAUDE_CODE_DISABLE_1M_CONTEXT=1` gives a 200k window, and then it compacts
  inside a turn too, between 175k and 190k.

The "compact at 40%" advice has no Anthropic source: no Claude Code doc gives
a percentage, and Anthropic's own guidance ("Effective context engineering
for AI agents") is to keep context small with subagents, notes and fresh
sessions, not a number. The 40% figure comes from talks (HumanLayer's "dumb
zone"); research such as Chroma's "Context Rot" shows quality drops as input
grows, with no single threshold. Compacting often also costs: every
compaction is a model call over the whole conversation, and it loses detail.

So Decurion sets `--autocompact 200000` (`AUTOCOMPACT_WINDOW`), the window
Claude Code itself uses for most models, and keeps the 1M window inside a
turn. Its pieces are small and each runs in a fresh session with a separate
checker, which is the structural version of the same advice. Each piece now
shows its longest conversation and how often it was compacted; if pieces
often go past 200k inside a turn, `CLAUDE_CODE_DISABLE_1M_CONTEXT=1` is the
next step.

## How tokens are counted

- A Claude Code `result` line has `usage` (only the turn that just ended) and
  `modelUsage` (the whole session, every model and subagent, carried across a
  resume like `total_cost_usd`). The runner reads `modelUsage`
  (`session_tokens`), and `ContextMeter` adds the biggest single request and
  the number of `compact_boundary` lines.
- pi reports per call; `PiRun` adds them up for the run.
- The hub (`crates/decurion-hub/src/tokens.rs`) keeps each run's and helper
  job's own share: a run or job marked `resumed` has the session's earlier
  rows taken off, as `prior_micros` does for money.

## Measuring again

After moving `CLAUDE_CODE_VERSION`, measure again with
`docs/spikes/prompt-size/`. It needs Node and Python, no account.

1. Install the pinned CLI and browser server in a scratch folder:
   `npm pack @anthropic-ai/claude-code-linux-x64@<version>` (untar it) and
   `npm install @playwright/mcp@<pinned version>` with
   `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1`.
2. Start the stand-in: `node capture.mjs <out> 31999`.
3. In a copy of an app's repo, run the CLI the way `run_args` does, with a
   fresh `HOME` and `CLAUDE_CONFIG_DIR`, `CLAUDE_CODE_OAUTH_TOKEN=x`,
   `ANTHROPIC_BASE_URL=http://127.0.0.1:31999`, `IS_SANDBOX=1`, an `--mcp-config`
   for the stdio browser, and the prompt on stdin.
4. `python3 -I measure.py <out> -v` prints the sizes and the tool names.

Set `FAKE_READ` to a big number to see where compaction starts (look for a
`compact_boundary` line in the CLI's output).
