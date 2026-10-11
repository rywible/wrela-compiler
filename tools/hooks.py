"""Claude Code's hooks for this repository, which .claude/settings.json runs. Each makes a rule of
CLAUDE.md that agents skipped into a check, or gives back what an agent loses to a compaction.

    python3 tools/hooks.py bash      before each Bash command (PreToolUse). Refuses waiting in
                                     the foreground on a loop of `sleep`, and a `cargo test` that
                                     takes the GPU alone; formats what the gate checks before the
                                     gate runs (`tools/check.sh --fmt`).
    python3 tools/hooks.py session   at a session's start (SessionStart): names the open
                                     milestone's task file, and after a compaction gives its text.
                                     On a cloud machine, also makes it a dev machine (cloud).

Each reads the hook's JSON on stdin. A refusal is its reason, for Claude, on stderr, and exit 2.
Anything else that goes wrong lets the command run: a hook mustn't stop work by its own fault.
"""

import glob
import json
import os
import re
import shlex
import subprocess
import sys

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# A loop that waits for something to finish by sleeping.
POLL = re.compile(r"\b(until|while)\b.*?\bdo\b.*?\bsleep\b", re.S)
# A command that starts by sleeping a while, to read something after.
NAP = re.compile(r"^\s*sleep\s+(\d+)")

# Where a command starts: at the start, after a separator or a keyword that runs one, past
# variables set for it and `time`. A path in a command's arguments isn't one.
START = r"(?:^|[;&|(\n]|\b(?:then|do|else)\b)\s*(?:\w+=\S*\s+)*(?:time\s+)?"

# Text a command only quotes, which runs nothing: a heredoc's body (a file written, a commit's
# message) and a string in single quotes.
HEREDOC = re.compile(r"<<-?\s*(['\"]?)(\w+)\1[^\n]*\n.*?\n[ \t]*\2[ \t]*(?=\n|$)", re.S)
QUOTED = re.compile(r"'[^']*'")


def code(command):
    """A command without the text it only quotes."""
    return QUOTED.sub("''", HEREDOC.sub("<<", command))

CARGO_TEST = re.compile(START + r"cargo\s+(?:\+\S+\s+)?test\b")
# Runs that check.sh can't make, or that don't use the GPU: setting `WRELA_GPU_SHARED` says the
# GPU is shared (or, at 1, taken alone on purpose); goldens rewritten, run cases narrowed (check.sh
# unsets both), fuzzing, builds, doc tests.
CARGO_TEST_OK = re.compile(r"WRELA_GPU_SHARED=|WRELA_BLESS|WRELA_RUN_ONLY|WRELA_FUZZ|--no-run\b|--doc\b")

CHECK = re.compile(START + r"[\w./-]*?tools/check\.sh\b([^;&|\n)]*)")

POLL_REASON = (
    "Don't wait in the foreground on a loop of `sleep`. Run the command in the background "
    "(run_in_background): it wakes you when it exits, so keep working or end your turn. To wait "
    "for a line in a log, use Monitor. (tools/hooks.py; CLAUDE.md, Checks)"
)
CARGO_TEST_REASON = (
    "A test run with `cargo test` takes the GPU alone, so every other session's checks wait for "
    "it, and it compiles the WASM again. Run it with `tools/check.sh <name> --nocapture` (add "
    "`--long` for a long test): it shares the GPU and keeps compiled code. A measurement "
    "(`measure:`) runs with cargo, alone; so does a run that sets `WRELA_GPU_SHARED` (=1 to take "
    "the GPU alone on purpose). (tools/hooks.py; CLAUDE.md, Checks)"
)


def repo(event):
    """The checkout the session works in: the one holding its `cwd` (a worktree's own), else
    this file's."""
    try:
        out = subprocess.run(
            ["git", "-C", event.get("cwd") or os.getcwd(), "rev-parse", "--show-toplevel"],
            capture_output=True, text=True, timeout=10,
        )
        if out.returncode == 0 and out.stdout.strip():
            return out.stdout.strip()
    except (OSError, subprocess.SubprocessError):
        pass
    return HERE


def polls(command, background):
    """Whether a command waits in the foreground for something to finish."""
    if background:
        return False
    nap = NAP.match(command)
    return bool(POLL.search(command) or (nap and int(nap.group(1)) >= 5))


def measures(root):
    """The names of the tests whose `#[ignore]` reason starts `measure:`, as check.sh finds them."""
    names = set()
    for top in ("compiler", "runtime"):
        for path in glob.glob(os.path.join(root, top, "**", "*.rs"), recursive=True):
            if f"{os.sep}target{os.sep}" in path:
                continue
            with open(path, encoding="utf-8", errors="replace") as f:
                lines = f.read().splitlines()
            for i, line in enumerate(lines):
                if '#[ignore = "measure:' in line:
                    for later in lines[i + 1:]:
                        if m := re.search(r"fn (\w+)\(", later):
                            names.add(m.group(1))
                            break
    return names


def takes_the_gpu_alone(command, root):
    """Whether a command runs tests with cargo that check.sh should run instead."""
    if not CARGO_TEST.search(command) or CARGO_TEST_OK.search(command):
        return False
    return not measures(root) & set(re.findall(r"\w+", command))


def runs_gate(command):
    """Whether a command runs the gate: check.sh with no filter (--long and --full run it too)."""
    for m in CHECK.finditer(command):
        words = []
        for arg in m.group(1).split():
            if arg[0] in "<>" or re.match(r"\d?&?>", arg):
                break
            words.append(arg)
        if "--fmt" not in words and all(w.startswith("-") for w in words):
            return True
    return False


def format_for_gate(root):
    """Formats what the gate checks; returns the files it changed, relative to the checkout."""
    out = subprocess.run(
        [os.path.join(root, "tools", "check.sh"), "--fmt"],
        cwd=root, capture_output=True, text=True, timeout=110,
    )
    files = []
    for line in out.stdout.splitlines():
        line = line.strip()
        if line:
            files.append(os.path.relpath(line, root) if os.path.isabs(line) else line)
    return files


def bash(event):
    tool_input = event.get("tool_input") or {}
    command = code(tool_input.get("command") or "")
    if polls(command, tool_input.get("run_in_background")):
        return refuse(POLL_REASON)
    root = None
    if CARGO_TEST.search(command):
        root = repo(event)
        if takes_the_gpu_alone(command, root):
            return refuse(CARGO_TEST_REASON)
    if runs_gate(command):
        files = format_for_gate(root or repo(event))
        if files:
            said = ", ".join(files)
            print(json.dumps({
                "systemMessage": f"Formatted before the gate: {said}",
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "additionalContext": f"tools/hooks.py formatted these files before the gate "
                                         f"ran (read them again before you edit them): {said}",
                },
            }))
    return 0


# SessionStart's text goes into the context up to 10,000 characters.
LIMIT = 9000


# What a session on a cloud machine is told.
CLOUD = """This is a Claude Code cloud machine: 4 cores, no GPU. Its GPU is the CPU (Mesa's lavapipe, \
patched and built by tools/cloud.sh): a floor frame at 1080p takes about 0.6 s, a lookbook still \
(`tools/lookbook.py <out> --stills <n>`, 301 frames) about 3 minutes, the gate about 20 minutes. \
Headless Chrome runs WebGPU on SwiftShader, many times slower again: prefer the native host's \
stills. {warm}"""


def cloud(root, source):
    """On a Claude Code cloud machine: gives the session's commands its variables (the env
    file), and at a new session's start (or on a machine the environment's setup script didn't
    provision) starts `tools/cloud.sh --warm` in the background: it provisions the machine if it
    isn't, then builds what a session builds first. Returns what the session is told."""
    script = os.path.join(root, "tools", "cloud.sh")
    env = subprocess.run([script, "--env"], capture_output=True, text=True, timeout=10).stdout
    path = os.environ.get("CLAUDE_ENV_FILE")
    if path:
        with open(path, "a", encoding="utf-8") as f:
            for line in env.splitlines():
                key, _, value = line.partition("=")
                f.write(f"export {key}={shlex.quote(value)}\n")
    provisioned = subprocess.run([script, "--check"]).returncode == 0
    if source != "startup" and provisioned:
        return CLOUD.format(warm="")
    subprocess.Popen([script, "--warm"], cwd=root, start_new_session=True,
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                     stderr=subprocess.DEVNULL)
    warm = ("The CLI, the native host, the floor's bake and the tests are building in the "
            "background (`tools/cloud.sh --warm`, log /tmp/wrela-warm.log): a cargo command waits "
            "for its lock meanwhile.")
    if not provisioned:
        warm = ("The environment's setup script didn't provision this machine, so it's being "
                "provisioned now (about 3 minutes): run `tools/cloud.sh` before anything that "
                "builds or renders; it waits for the provisioning. " + warm)
    return CLOUD.format(warm=warm)


def session(event):
    root = repo(event)
    if os.environ.get("CLAUDE_CODE_REMOTE") == "true":
        print(cloud(root, event.get("source")))
    for path in sorted(glob.glob(os.path.join(root, "M*-TASKS.md"))):
        name = os.path.basename(path)
        if event.get("source") != "compact":
            print(f"{name} is the open milestone's task file: read it before milestone work.")
            continue
        with open(path, encoding="utf-8") as f:
            text = f.read()
        if len(text) > LIMIT:
            # Cut at a heading, and say where the rest is.
            cut = text.rfind("\n## ", 0, LIMIT)
            text = text[: cut if cut > 0 else LIMIT] + f"\n\n(The rest is in {name}.)"
        print(f"The open milestone's task file, {name}, as it is now:\n\n{text}")
    return 0


def refuse(reason):
    print(reason, file=sys.stderr)
    return 2


def main(argv):
    hooks = {"bash": bash, "session": session}
    if len(argv) != 2 or argv[1] not in hooks:
        print(f"usage: {argv[0]} {' | '.join(hooks)}", file=sys.stderr)
        return 1
    try:
        return hooks[argv[1]](json.load(sys.stdin))
    except Exception as e:  # A broken hook lets the command run, and says why.
        print(f"tools/hooks.py {argv[1]}: {e!r}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
