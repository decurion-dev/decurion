# Spike: Claude Code in a container on Windows (2026-10-17)

Host: Windows 11 Pro 26200, Docker Desktop 29.6.2 (Linux engine), Claude Code 2.1.289.
Script: `spike/windows_container_spike.py`. Result: **pass**.

| Check | Result |
| --- | --- |
| Runs against a bind-mounted folder whose path contains a space | yes |
| Killed mid-task (`docker kill` after step 2 of 6, exit 137) | resumed with `--resume <session>` in a fresh container, same session id |
| Steps redone after the kill | none: step1..step6 each committed exactly once |
| Decoy secrets in the launching process env (`GH_TOKEN`, `AWS_SECRET_ACCESS_KEY`) | not visible in the container env |
| Decoy `.env` next to the worktree | not readable (only the worktree is mounted) |
| `~/.ssh`, host `~/.claude`, MCP OAuth tokens | absent |
| Hub-side push replayed after a simulated crash | second attempt saw the remote already at the commit and skipped |
| Elapsed | 57 s |

## Findings that changed the design

1. **Never mount or copy the host `~/.claude/.credentials.json`.** It holds the
   Claude login plus MCP OAuth tokens for other services (Sentry, Supabase,
   Vercel, Stripe, Cloudflare). Its access token was also rejected as revoked
   inside the container, and sharing its refresh token would fight the host
   session. Each profile instead uses a long-lived token from
   `claude setup-token`, kept in Windows Credential Manager as
   `decurion/claude-profile-<A|B>` and passed to `docker run` by name only.
2. **The Claude token is the one credential inside the container.** It is in
   the agent's environment by necessity, so anything the agent runs can read
   it. Everything else (GitHub, AWS, app secrets) stays with the hub.
3. **`git worktree` does not work across the container boundary.** A
   worktree's `.git` file points at the main repo's host path, which does not
   exist in the container, so the agent cannot commit. Found in the first
   end-to-end run; the runner now makes a self-contained `git clone --local`
   per thread.
4. **Agents can report success without doing the work.** The same run ended
   with `is_error: false` although nothing was committed. The runner now
   moves work to review only when the branch has new commits.
5. Pipe both stdout and stderr, or neither: an unread pipe stalled the first
   spike attempt.
