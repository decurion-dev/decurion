"""Windows spike: Claude Code in a sealed container, killed mid-run, resumed.

Checks:
  1. Claude Code runs in a container on a Windows host against a bind-mounted
     worktree whose path contains a space.
  2. No host secret reaches the container (decoys planted in the launching
     process environment and next to the worktree).
  3. Killing the container mid-task and resuming the same session finishes the
     task without redoing finished steps.
  4. A hub-side push replayed after a "crash" is a no-op.

Usage: python spike/windows_container_spike.py <scratch-dir>
The Claude token is read from the host credentials file and passed by name
only (never on a command line, never printed).
"""

import json
import os
import subprocess
import sys
import time
import uuid
from pathlib import Path

IMAGE = "decurion/claude-worker:dev"
STEPS = 6
KILL_AFTER = 2
DECOY = "DECOY_7F3K_SHOULD_NOT_LEAK"


def sh(args, cwd=None, env=None, check=True):
    r = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True,
                       encoding="utf-8", errors="replace")
    if check and r.returncode != 0:
        raise RuntimeError(f"{args[:3]} failed: {r.stderr.strip()[:400]}")
    return r


def commits(work):
    out = sh(["git", "log", "--format=%s"], cwd=work).stdout.split()
    return [c for c in out if c.startswith("step")]


def claude_token():
    # Per-profile token from `claude setup-token`, kept in Credential Manager.
    # The host ~/.claude/.credentials.json is never used: it also carries MCP
    # OAuth tokens for other services.
    sys.path.insert(0, str(Path(__file__).parent))
    import wincred
    return wincred.read(os.environ.get("DECURION_PROFILE", "A"))


def docker_env(token):
    # The launching process carries decoys, to prove the container does not
    # inherit the host environment.
    env = dict(os.environ)
    env.update({"GH_TOKEN": DECOY, "AWS_SECRET_ACCESS_KEY": DECOY,
                "CLAUDE_CODE_OAUTH_TOKEN": token})
    return env


def run_args(name, work, home_vol, session, prompt, resume):
    a = ["docker", "run", "--rm", "--name", name,
         "--mount", f"type=bind,src={work},dst=/work",
         "--mount", f"type=volume,src={home_vol},dst=/home/agent",
         "--memory", "2g", "--pids-limit", "512",
         "-e", "CLAUDE_CODE_OAUTH_TOKEN",
         "-e", "GIT_CONFIG_COUNT=1",
         "-e", "GIT_CONFIG_KEY_0=safe.directory",
         "-e", "GIT_CONFIG_VALUE_0=*",
         "-e", "GIT_AUTHOR_NAME=decurion-agent", "-e", "GIT_AUTHOR_EMAIL=agent@decurion.invalid",
         "-e", "GIT_COMMITTER_NAME=decurion-agent", "-e", "GIT_COMMITTER_EMAIL=agent@decurion.invalid",
         IMAGE, "-p", prompt, "--output-format", "stream-json", "--verbose",
         "--permission-mode", "bypassPermissions", "--max-turns", "60"]
    a += ["--resume", session] if resume else ["--session-id", session]
    return a


def main():
    root = Path(sys.argv[1]).resolve() / "spike run"   # space on purpose
    root.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%H%M%S")
    origin = root / f"origin-{stamp}.git"
    work = root / f"work-{stamp}"
    (root / ".env").write_text(f"SECRET={DECOY}\n")  # decoy beside the worktree

    sh(["git", "init", "--bare", "-q", "-b", "main", str(origin)])
    sh(["git", "clone", "-q", str(origin), str(work)])
    (work / "README.md").write_text("spike\n")
    sh(["git", "add", "-A"], cwd=work)
    sh(["git", "-c", "user.name=spike", "-c", "user.email=s@x.invalid",
        "commit", "-qm", "init"], cwd=work)
    sh(["git", "push", "-q", "origin", "HEAD:main"], cwd=work)

    token = claude_token()
    env = docker_env(token)
    home_vol = f"decurion-spike-home-{stamp}"
    sh(["docker", "volume", "create", home_vol])
    session = str(uuid.uuid4())
    result = {"session": session, "work": str(work)}

    task = (f"Do these {STEPS} steps strictly in order, one at a time. For step N "
            f"(N = 1..{STEPS}): if a commit with message 'stepN' already exists in "
            "`git log`, skip it. Otherwise create file stepN.txt containing 'N', run "
            "`sleep 6`, then run `git add -A && git commit -m stepN`. Use one shell "
            "command per step. Do not push. When all steps are done reply DONE.")

    # Run 1: kill it once KILL_AFTER steps are committed.
    name1 = f"decurion-spike-{stamp}-a"
    t0 = time.time()
    log1 = open(root / f"run1-{stamp}.jsonl", "w", encoding="utf-8")
    p = subprocess.Popen(run_args(name1, work, home_vol, session, task, False),
                         env=env, stdout=log1, stderr=subprocess.STDOUT)
    while time.time() - t0 < 30 and not sh(["docker", "ps", "-q", "-f", f"name={name1}"]).stdout.strip():
        time.sleep(0.5)
    time.sleep(2)
    # Leak scan while the agent is live.
    leak = sh(["docker", "exec", name1, "sh", "-c",
               f"env | grep -c {DECOY}; grep -rl {DECOY} / --exclude-dir=proc --exclude-dir=sys 2>/dev/null | head -5; "
               "ls -a /home/agent; ls / ; test -e /home/agent/.ssh && echo HAS_SSH; "
               "grep -l mcpOAuth -r /home/agent 2>/dev/null | head -3"],
              check=False)
    result["leak_scan"] = leak.stdout.strip().splitlines()
    killed_at = None
    while time.time() - t0 < 600:
        if len(commits(work)) >= KILL_AFTER:
            sh(["docker", "kill", name1], check=False)
            killed_at = commits(work)
            break
        if p.poll() is not None:
            break
        time.sleep(1)
    p.wait(timeout=60)
    result["killed_after_commits"] = killed_at
    result["run1_exit"] = p.returncode
    log1.close()
    result["run1_log_tail"] = (root / f"run1-{stamp}.jsonl").read_text(encoding="utf-8")[-300:]

    # Run 2: resume the same session in a fresh container.
    name2 = f"decurion-spike-{stamp}-b"
    r2 = subprocess.run(run_args(name2, work, home_vol, session,
                                 "The container restarted. Continue the same task from where it stopped.", True),
                        env=env, capture_output=True, text=True, encoding="utf-8",
                        errors="replace", timeout=900)
    last = [json.loads(l) for l in r2.stdout.splitlines() if l.startswith("{")]
    final = next((e for e in reversed(last) if e.get("type") == "result"), {})
    result["run2_exit"] = r2.returncode
    result["run2_result"] = str(final.get("result", ""))[:200]
    result["run2_resumed_same_session"] = final.get("session_id") == session
    result["run2_stderr_tail"] = r2.stderr[-300:]
    result["final_commits"] = list(reversed(commits(work)))
    result["duplicate_steps"] = sorted({c for c in commits(work) if commits(work).count(c) > 1})

    # Hub-side push, replayed: the second attempt must be a no-op.
    head = sh(["git", "rev-parse", "HEAD"], cwd=work).stdout.strip()
    pushes = []
    for attempt in (1, 2):
        remote = sh(["git", "ls-remote", str(origin), "refs/heads/agent-work"], cwd=work).stdout.split()
        if remote and remote[0] == head:
            pushes.append(f"attempt {attempt}: already at {head[:8]}, skipped")
            continue
        sh(["git", "push", "-q", str(origin), f"{head}:refs/heads/agent-work"], cwd=work)
        pushes.append(f"attempt {attempt}: pushed {head[:8]}")
    result["push_replay"] = pushes
    result["elapsed_s"] = round(time.time() - t0)

    sh(["docker", "volume", "rm", "-f", home_vol], check=False)
    print(json.dumps(result, indent=2))
    problems = []
    if result["leak_scan"][:1] != ["0"] or any(DECOY in l or l == "HAS_SSH" or "mcpOAuth" in l
                                              for l in result["leak_scan"][1:]):
        problems.append("possible leak")
    if not result["killed_after_commits"]:
        problems.append("run was never killed mid-task")
    if not result["run2_resumed_same_session"]:
        problems.append("did not resume the same session")
    if result["final_commits"] != [f"step{i}" for i in range(1, STEPS + 1)]:
        problems.append("steps missing or out of order")
    if result["duplicate_steps"]:
        problems.append("steps repeated")
    if not result["push_replay"][1].endswith("skipped"):
        problems.append("push replay was not a no-op")
    if problems:
        sys.exit("SPIKE FAILED: " + "; ".join(problems))


if __name__ == "__main__":
    main()
