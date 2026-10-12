#!/usr/bin/env python3
"""One machine-wide queue for costly local work.

    python3 tools/jobs.py run focused -- cargo check -p wrela-syntax
    python3 tools/jobs.py run capture -- tools/lookbook.py target/lookbook --stills 0
    python3 tools/jobs.py run measure -- cargo test --release --workspace -- --ignored --exact NAME
    python3 tools/jobs.py status
    python3 tools/jobs.py cancel ID

The caller waits with its agent's background-command mechanism. Leases survive in the command
if its supervisor is killed; queue files alone never count as a live job. Nested tools share
the existing admission only when the calling process is a descendant of its live owner.
"""

import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import time
import uuid

from resources import cpu_count, memory_pressure, resource_dir, worker_count

PRIORITY = {"focused": 0, "capture": 0, "build": 1, "gate": 2,
            "measure": 2, "long": 3, "background": 4}
TIMEOUT = {"focused": 900, "capture": 900, "build": 3600, "gate": 3600,
           "measure": 14400, "long": 14400, "background": 7200}


def _open(path):
    return os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)


@contextmanager
def locked(root):
    fd = _open(root / "queue.lock")
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        os.close(fd)


def _write(path, value):
    temporary = path.with_suffix(f".{os.getpid()}.tmp")
    with os.fdopen(_open(temporary), "w") as output:
        output.truncate(0)
        output.write(json.dumps(value))
    temporary.replace(path)


def _live(path):
    fd = _open(path)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return False
        except BlockingIOError:
            return True
    finally:
        os.close(fd)


def entries(root):
    """Called under the queue lock. Abandoned leases cannot block future work."""
    result = []
    for path in root.glob("job-*.json"):
        lease = path.with_suffix(".lease")
        if not _live(lease):
            path.unlink(missing_ok=True)
            lease.unlink(missing_ok=True)
            continue
        result.append(json.loads(path.read_text()))
    return result


def _parents():
    result = subprocess.run(["ps", "-A", "-o", "pid=,ppid="],
                            capture_output=True, text=True, check=True)
    return {int(pid): int(parent) for pid, parent in (line.split() for line in result.stdout.splitlines())}


def admitted():
    identifier = os.environ.get("WRELA_JOB_ID", "")
    if not identifier or not identifier.isalnum():
        return False
    root = resource_dir()
    with locked(root):
        job = next((j for j in entries(root) if j["id"] == identifier), None)
    if not job or job["state"] != "running":
        return False
    parents, pid = _parents(), os.getpid()
    while pid > 1:
        # The command keeps its inherited lease if a supervisor is killed. Its descendants
        # must keep using that admission rather than queue behind their own live reservation.
        if pid in (job["owner"], job.get("worker")):
            from worktrees import checkout_root, common_dir
            expected = checkout_root(job["cwd"])
            actual = checkout_root(os.getcwd())
            if actual != expected and (common_dir(actual) is not None or not actual.is_relative_to(expected)):
                raise ValueError(f"an admitted job must stay in checkout {expected}; "
                                 f"queue work in {actual} separately")
            return True
        pid = parents.get(pid, 1)
    return False


def fingerprint(cwd):
    """Include input content and file revisions, so editing then restoring still invalidates a pass."""
    from worktrees import checkout_root
    cwd = checkout_root(cwd)
    try:
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=cwd, stderr=subprocess.DEVNULL).strip().decode()
        digest = hashlib.sha256()
        def include(value):
            digest.update(len(value).to_bytes(8, "big"))
            digest.update(value)
        include(subprocess.check_output(["git", "diff", "HEAD", "--binary"], cwd=cwd))
        names = subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard", "-z"], cwd=cwd)
        for name in names.split(b"\0"):
            if name:
                path = Path(cwd) / os.fsdecode(name)
                include(name)
                if path.is_symlink():
                    include(os.fsencode(os.readlink(path)))
                elif path.is_file():
                    include(path.read_bytes())
        # Contents alone cannot prove a frozen source tree: a command may have compiled an
        # intermediate edit that was restored before it finished. ctime also records a write
        # whose mtime was restored. Keep this separate from the input content digest.
        revisions = hashlib.sha256()
        tracked = subprocess.check_output(["git", "ls-files", "--cached", "-z"], cwd=cwd)
        for name in sorted(set((tracked + names).split(b"\0")) - {b""}):
            revisions.update(len(name).to_bytes(8, "big"))
            revisions.update(name)
            try:
                stat = (Path(cwd) / os.fsdecode(name)).lstat()
                revision = [stat.st_ino, stat.st_mode, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns]
            except FileNotFoundError:
                revision = None
            revisions.update(json.dumps(revision).encode())
        return {"commit": head, "inputs": digest.hexdigest(), "revisions": revisions.hexdigest()}
    except subprocess.CalledProcessError:
        return {"commit": None, "inputs": None}


def next_job(jobs, now):
    waiting = [j for j in jobs if j["state"] == "queued"]
    return min(waiting, key=lambda j: (max(0, j["priority"] - (now - j["created"]) // 60),
                                       j["created"], j["id"]), default=None)


def can_start(kind, pressure):
    return pressure == 0 or pressure == 1 and kind in ("focused", "build", "gate")


def _tree(proc, known=None):
    """Remember owned descendants and their start times before an exiting parent is reaped."""
    result = subprocess.run(["ps", "-A", "-o", "pid=,ppid=,pgid=,stat=,lstart="],
                            capture_output=True, text=True, check=True)
    current = {}
    for line in result.stdout.splitlines():
        pid, parent, group, state, started = line.split(maxsplit=4)
        if state.startswith("Z"):
            continue
        current[int(pid)] = (int(parent), int(group), started)
    known = dict(known or {})
    owned = {proc.pid} | {pid for pid, item in known.items() if pid in current and current[pid][1:] == item[1:]}
    while True:
        more = {pid for pid, (parent, group, _) in current.items() if parent in owned or group == proc.pid}
        if more <= owned:
            break
        owned |= more
    known.update({pid: item for pid, item in current.items() if pid in owned})
    return known, current


def _stop(proc, known=None):
    """Stop the owned tree, including descendants such as Chrome with their own sessions."""
    for sig in (signal.SIGTERM, signal.SIGKILL):
        known, current = _tree(proc, known)
        groups = {item[1] for pid, item in known.items() if pid in current and current[pid][1:] == item[1:]}
        for group in groups:
            try:
                os.killpg(group, sig)
            except ProcessLookupError:
                pass
        try:
            proc.wait(timeout=3 if sig == signal.SIGTERM else 5)
        except subprocess.TimeoutExpired:
            continue
        # Descendants in their own sessions may still be shutting down.
        if sig == signal.SIGTERM:
            time.sleep(.1)
    proc.wait()


def run(kind, command, timeout=None, key=None, verify=False, maintenance=True):
    if admitted():
        return subprocess.call(command)
    root = resource_dir()
    identifier = uuid.uuid4().hex
    path = root / f"job-{identifier}.json"
    lease = _open(path.with_suffix(".lease"))
    fcntl.flock(lease, fcntl.LOCK_EX)
    cwd = os.getcwd()
    job = {"id": identifier, "owner": os.getpid(), "kind": kind, "command": command,
           "cwd": cwd, "state": "queued", "created": time.time(),
           "priority": PRIORITY[kind], "key": key, "source": fingerprint(cwd)}
    cancelled = False
    previous = {}

    def interrupt(signum, frame):
        nonlocal cancelled
        cancelled = True

    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        previous[sig] = signal.signal(sig, interrupt)
    proc, status, reason, descendants = None, 1, None, {}
    try:
        with locked(root):
            jobs = entries(root)
            if key:
                for old in jobs:
                    if old["state"] == "queued" and old["cwd"] == cwd and old.get("key") == key:
                        old["cancelled"] = True
                        _write(root / f"job-{old['id']}.json", old)
            _write(path, job)
        print(f"queued {identifier[:8]}: {kind} ({cwd})", file=sys.stderr, flush=True)
        while not cancelled:
            with locked(root):
                jobs = entries(root)
                mine = next(j for j in jobs if j["id"] == identifier)
                if mine.get("cancelled"):
                    cancelled = True
                    break
                pressure = memory_pressure() if not any(j["state"] == "running" for j in jobs) else 2
                # A deferred render must not keep a small check from its reduced allowance.
                first = next_job([j for j in jobs if can_start(j["kind"], pressure)], time.time())
                if first and first["id"] == identifier:
                    job["state"] = "running"
                    job["started"] = time.time()
                    job["pressure_at_start"] = pressure
                    _write(path, job)
                    break
            time.sleep(.2)
        if cancelled:
            print(f"{identifier[:8]}: cancelled before admission", file=sys.stderr)
            return 130
        from worktrees import checkout_root, environment, finish_cargo_cache, maintain, prepare_cargo_cache
        env = {**os.environ, **environment(cwd)}
        if maintenance:
            maintain(checkout_root(cwd))
        workers = cpu_count() if kind == "measure" else 1 if job["pressure_at_start"] else min(2, worker_count())
        env.update(WRELA_JOB_ID=identifier, WRELA_CPU_WORKERS=str(workers),
                   WRELA_TEST_PROGRAMS="1", RAYON_NUM_THREADS=str(workers),
                   RUST_TEST_THREADS="1" if kind == "measure" else str(workers))
        # Sharing is an explicit choice inside the correctness runner; a stale shell value
        # must not make a large standalone render or measurement share the GPU.
        env.pop("WRELA_GPU_SHARED", None)
        env["CARGO_BUILD_JOBS"] = str(min(1 if job["pressure_at_start"] else 4, cpu_count(), max(1, int(env.get("CARGO_BUILD_JOBS", "4")))))
        if env.get("WRELA_SOFTWARE_GPU"):
            env["LP_NUM_THREADS"] = "2"
        # The inputs checked are those at admission, after any wait and cache maintenance.
        job["source"] = fingerprint(cwd)
        cargo_cache = prepare_cargo_cache(cwd, env["CARGO_TARGET_DIR"], job["source"])
        with locked(root):
            if cancelled or json.loads(path.read_text()).get("cancelled"):
                print(f"{identifier[:8]}: cancelled before command start", file=sys.stderr)
                return 130
            _write(path, job)
        print(f"running {identifier[:8]} after {time.time() - job['created']:.1f}s in queue", file=sys.stderr)
        launch = command if kind == "measure" else ["nice", "-n", "5", *command]
        proc = subprocess.Popen(launch, env=env, start_new_session=True, pass_fds=(lease,))
        job["worker"] = proc.pid
        with locked(root):
            _write(path, job)
        started, pressure, sampled = time.monotonic(), 0, 0
        limit = TIMEOUT[kind] if timeout is None else timeout
        while proc.poll() is None:
            descendants, _ = _tree(proc, descendants)
            with locked(root):
                current = json.loads(path.read_text())
            if current.get("cancelled"):
                cancelled = True
            if cancelled:
                status, reason = 130, "cancelled"
                break
            if time.monotonic() - started >= limit:
                status, reason = 124, f"timed out after {limit}s"
                break
            if time.monotonic() - sampled >= 1:
                pressure = pressure + 1 if memory_pressure() == 2 else 0
                sampled = time.monotonic()
            if pressure >= 3:
                status, reason = 75, "stopped under sustained critical memory pressure"
                break
            if Path(env["CARGO_TARGET_DIR"]).exists() and \
                    shutil.disk_usage(env["CARGO_TARGET_DIR"]).free < 4 * 1024**3:
                status, reason = 75, "stopped to retain 4 GiB free disk space"
                break
            time.sleep(.2)
        else:
            status = proc.returncode if proc.returncode >= 0 else 128 - proc.returncode
        if reason:
            print(f"{identifier[:8]}: {reason}", file=sys.stderr)
            _stop(proc, descendants)
        else:
            # A successful command must not leave its previews or watchers running after its
            # admission has been returned. Only descendants observed as ours are stopped.
            _stop(proc, descendants)
        after = fingerprint(cwd)
        finish_cargo_cache(cargo_cache, after)
        if verify and status == 0 and after != job["source"]:
            status = 1
            print("source inputs changed during this job; its success does not verify the current tree", file=sys.stderr)
        history = root / "history"
        history.mkdir(exist_ok=True)
        job.update(state="finished", status=status, reason=reason, finished=time.time(), source_after=after)
        _write(history / f"{identifier}.json", job)
        for old in sorted(history.glob("*.json"), key=lambda p: p.stat().st_mtime)[:-100]:
            old.unlink()
        return status
    finally:
        if proc is not None and proc.poll() is None:
            _stop(proc, descendants)
        with locked(root):
            path.unlink(missing_ok=True)
        os.close(lease)
        path.with_suffix(".lease").unlink(missing_ok=True)
        for sig, handler in previous.items():
            signal.signal(sig, handler)


def check(arguments):
    """One verified admission for the suite or one named check."""
    from check import selection
    name = selection(arguments)
    script = str(Path(__file__).with_name("check.sh").resolve())
    return run("focused" if name else "gate", ["bash", script, *arguments], verify=True)


def main():
    if sys.argv[1:2] == ["admitted"]:
        return 0 if admitted() else 1
    if sys.argv[1:2] == ["check"]:
        return check(sys.argv[2:])
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="action", required=True)
    execution = sub.add_parser("run")
    execution.add_argument("kind", choices=PRIORITY)
    execution.add_argument("--timeout", type=float)
    execution.add_argument("--key")
    execution.add_argument("--verify", action="store_true", help="fail if source inputs change during the command")
    sub.add_parser("status")
    cancellation = sub.add_parser("cancel")
    cancellation.add_argument("id")
    argv = sys.argv[1:]
    command = []
    if argv[:1] == ["run"]:
        if "--" not in argv:
            parser.error("separate the command with --")
        separator = argv.index("--")
        command, argv = argv[separator + 1:], argv[:separator]
    args = parser.parse_args(argv)
    if args.action == "run":
        if not command or (args.timeout is not None and (args.timeout <= 0 or not math.isfinite(args.timeout))):
            parser.error("a command and a positive finite timeout are required")
        return run(args.kind, command, args.timeout, args.key, args.verify)
    root = resource_dir()
    with locked(root):
        jobs = entries(root)
        if args.action == "cancel":
            matches = [j for j in jobs if j["id"].startswith(args.id)]
            if len(matches) != 1:
                parser.error("job id must identify exactly one live job")
            matches[0]["cancelled"] = True
            _write(root / f"job-{matches[0]['id']}.json", matches[0])
        else:
            pressure = memory_pressure()
            if pressure:
                print("admission paused: critical memory pressure" if pressure == 2 else
                      "warning memory pressure: checks/builds use one worker; renders and long work wait")
            for job in sorted(jobs, key=lambda j: j["created"]):
                print(f"{job['id'][:8]} {job['state']:7} {job['kind']:10} {job['cwd']}")
            if not jobs:
                print("no queued or running jobs")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"jobs: {error}")
