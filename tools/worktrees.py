#!/usr/bin/env python3
"""Small source worktrees, one scheduled Cargo cache, and safe disposable-output cleanup.

    python3 tools/worktrees.py create NAME [--ref main]
    python3 tools/worktrees.py status
    python3 tools/worktrees.py remove NAME
    python3 tools/worktrees.py prune [--build-cache]
    python3 tools/worktrees.py env

No build tree or node_modules is copied when creating a session. Removal deletes the worktree
and local branch only when merged, including squash merges; dirty trees and needed outputs
are preserved. Prune never deletes a worktree, source, or capture.
"""

import argparse
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import time
import tomllib

GIB = 1024**3
MIN_FREE = 12 * GIB
CACHE_BUDGET = 20 * GIB


def common_dir(cwd):
    result = subprocess.run(["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
                            cwd=cwd, capture_output=True, text=True)
    return Path(result.stdout.strip()) if result.returncode == 0 else None


def checkout_root(cwd):
    result = subprocess.run(["git", "rev-parse", "--show-toplevel"],
                            cwd=cwd, capture_output=True, text=True)
    return Path(result.stdout.strip()) if result.returncode == 0 else Path(cwd).resolve()


def environment(cwd):
    common = common_dir(cwd)
    if common is None:
        return {"CARGO_TARGET_DIR": os.environ.get("CARGO_TARGET_DIR", str(Path(cwd) / "target"))}
    cache = common / "wrela/cache"
    # Explicit custom targets remain useful for fixtures and deliberate cold measurements.
    return {"CARGO_TARGET_DIR": os.environ.get("CARGO_TARGET_DIR", str(cache / "cargo")),
            "WRELA_NATIVE_CACHE": os.environ.get("WRELA_NATIVE_CACHE", str(cache / "native")),
            "WRELA_CONST_CACHE": os.environ.get("WRELA_CONST_CACHE", str(cache / "const"))}


def cargo_workspace_root(cwd):
    """Find the enclosing workspace, respecting a nested or excluded fixture package."""
    nearest = None
    for root in [Path(cwd).resolve(), *Path(cwd).resolve().parents]:
        manifest = root / "Cargo.toml"
        if not manifest.is_file():
            continue
        nearest = nearest or root
        data = tomllib.loads(manifest.read_text())
        if "workspace" in data:
            excluded = [path for pattern in data["workspace"].get("exclude", [])
                        for path in root.glob(pattern)]
            return nearest if any(nearest.is_relative_to(path) for path in excluded) else root
    return nearest


def cargo_workspace_packages(cwd):
    """Package names, not target names or a prefix shared by unrelated dependencies."""
    root = cargo_workspace_root(cwd)
    if root is None:
        return []
    manifest = root / "Cargo.toml"
    workspace = tomllib.loads(manifest.read_text())
    manifests = {manifest} if "package" in workspace else set()
    for member in workspace.get("workspace", {}).get("members", []):
        manifests.update(path / "Cargo.toml" for path in root.glob(member))
    return sorted({tomllib.loads(path.read_text())["package"]["name"] for path in manifests})


def cargo_workspace_fingerprints(target, packages):
    """Workspace fingerprints in every profile, including cross-compiled targets."""
    directories = [*target.glob("*/.fingerprint"), *target.glob("*/*/.fingerprint")]
    return sorted(path for directory in directories for path in directory.iterdir()
                  if path.is_dir() and not path.is_symlink()
                  and path.name.rsplit("-", 1)[0] in packages)


def cargo_artifact_signature(target, packages):
    # Cargo can overwrite an entry with identical fingerprint bytes from another checkout.
    # Its write times/inodes reveal this even when that checkout predates this guard.
    result = {}
    for directory in cargo_workspace_fingerprints(target, packages):
        for path in sorted(directory.rglob("*")):
            if path.is_file():
                stat = path.stat()
                result[str(path.relative_to(target))] = [stat.st_ino, stat.st_size, stat.st_mtime_ns]
    return result


def prepare_cargo_cache(cwd, target, source):
    """Under job admission, reject workspace artifacts from different or unknown inputs.

    Cargo's relative paths and mtime checks can accept an older checkout's different source.
    Keep compiled dependencies and incremental data; remove only workspace freshness records
    when changing inputs, checkout, or after an unrecorded writer touched those records.
    """
    packages = cargo_workspace_packages(cwd)
    if not packages:
        return None
    target = Path(target)
    target.mkdir(parents=True, exist_ok=True)
    record = target / ".wrela-workspace.json"
    try:
        previous = json.loads(record.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        previous = {}
    identity = {"checkout": str(cargo_workspace_root(cwd)), "source": source, "packages": packages}
    # Include removed/renamed packages whose old records may still be in this shared target.
    all_packages = set(packages) | set(previous.get("identity", {}).get("packages", []))
    artifacts = cargo_artifact_signature(target, all_packages)
    if source.get("inputs") is None or previous.get("identity") != identity \
            or previous.get("artifacts") != artifacts:
        for directory in cargo_workspace_fingerprints(target, all_packages):
            shutil.rmtree(directory)
    # A killed job must not leave a record that certifies artifacts it might have replaced.
    record.unlink(missing_ok=True)
    return {"target": target, "record": record, "identity": identity}


def finish_cargo_cache(state, source):
    """Record only a completed job's unchanged inputs; never record a test pass here."""
    if state is None or source.get("inputs") is None or state["identity"]["source"] != source:
        return
    record = state["record"]
    # A cache-prune job may have removed the target itself.
    record.parent.mkdir(parents=True, exist_ok=True)
    data = {"identity": state["identity"],
            "artifacts": cargo_artifact_signature(state["target"], state["identity"]["packages"])}
    temporary = record.with_suffix(".tmp")
    temporary.write_text(json.dumps(data, sort_keys=True) + "\n")
    temporary.replace(record)


def allocated(path):
    """Allocated bytes, counting hard links once and never traversing symlinks."""
    seen, total = set(), 0
    for root, directories, files in os.walk(path):
        directories[:] = [d for d in directories if not (Path(root) / d).is_symlink()]
        for name in files:
            try:
                stat = (Path(root) / name).lstat()
            except FileNotFoundError:
                continue
            key = stat.st_dev, stat.st_ino
            if key not in seen:
                seen.add(key)
                total += stat.st_blocks * 512
    return total


def _alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def prune_scratch(root, now=None):
    """Only failed/abandoned test scratch, after six hours and its owner has exited."""
    now = time.time() if now is None else now
    for path in Path(root).glob("test-scratch-*"):
        if path.is_symlink() or not path.is_dir() or now - path.stat().st_mtime <= 6 * 3600:
            continue
        try:
            if _alive(int((path / ".owner").read_text())):
                continue
        except (FileNotFoundError, ValueError):
            # Old scratch without ownership is retained: age alone isn't an ownership proof.
            continue
        shutil.rmtree(path)


def maintain(cwd, clear_build=False):
    common = common_dir(cwd)
    target = Path(environment(cwd)["CARGO_TARGET_DIR"])
    target.mkdir(parents=True, exist_ok=True)
    if common:
        cache = common / "wrela/cache"
        cache.mkdir(parents=True, exist_ok=True)
        if clear_build:
            cargo = cache / "cargo"
            if cargo.exists():
                shutil.rmtree(cargo)
            cargo.mkdir()
        # Shared content-keyed caches can be regenerated; captured evidence isn't a cache.
        cutoff = time.time() - 7 * 86400
        for directory in (cache / "native", cache / "const"):
            if directory.is_symlink():
                raise ValueError(f"cache directory must not be a symlink: {directory}")
            for path in directory.glob("*"):
                if path.is_file() and not path.is_symlink() and path.stat().st_mtime < cutoff:
                    path.unlink()
        cargo = cache / "cargo"
        # Incremental state is disposable, and costs less to rebuild than all dependencies.
        allocated_bytes = allocated(cache)
        if allocated_bytes > CACHE_BUDGET or shutil.disk_usage(target).free < MIN_FREE:
            removed = False
            for path in cargo.glob("**/incremental"):
                if path.is_dir() and not path.is_symlink():
                    shutil.rmtree(path)
                    removed = True
            if removed:
                allocated_bytes = allocated(cache)
        if allocated_bytes > CACHE_BUDGET:
            raise ValueError("shared caches exceed 20 GiB; run tools/worktrees.py prune --build-cache")
    prune_scratch(Path(cwd) / "target")
    if shutil.disk_usage(target).free < MIN_FREE:
        raise ValueError("less than 12 GiB free; inspect tools/worktrees.py status and prune disposable build output")


def _registry(common):
    path = common / "wrela/worktrees.json"
    return path, json.loads(path.read_text()) if path.exists() else {}


def _save(path, registry):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(f".{os.getpid()}.tmp")
    temporary.write_text(json.dumps(registry, indent=2) + "\n")
    temporary.replace(path)


@contextmanager
def registry_lock(cwd):
    common = common_dir(cwd)
    if common is None:
        raise ValueError("worktrees require a Git repository")
    directory = common / "wrela"
    directory.mkdir(exist_ok=True)
    with open(directory / "worktrees.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def create(cwd, name, ref="main"):
    with registry_lock(cwd):
        return _create(cwd, name, ref)


def _create(cwd, name, ref):
    if not re.fullmatch(r"[a-z][a-z0-9-]{0,47}", name):
        raise ValueError("use a lowercase name of up to 48 letters, digits and hyphens")
    common = common_dir(cwd)
    if common is None:
        raise ValueError("worktrees require a Git repository")
    path, registry = _registry(common)
    if name in registry or len(registry) >= 10:
        raise ValueError("name already exists or ten session worktrees are registered")
    if shutil.disk_usage(common).free < MIN_FREE:
        raise ValueError("less than 12 GiB free; prune disposable output before creating a worktree")
    # Outside every checkout, and separate from Git's own worktree metadata.
    root = common.parent.parent / f".{common.parent.name}-sessions"
    root.mkdir(exist_ok=True)
    destination = root / name
    branch = f"codex/{name}"
    subprocess.run(["git", "worktree", "add", "-b", branch, str(destination), ref], cwd=cwd, check=True)
    registry[name] = {"path": str(destination), "branch": branch, "created": time.time()}
    _save(path, registry)
    return destination


def remove(cwd, name):
    with registry_lock(cwd):
        _remove(cwd, name)


def merged_head(cwd, branch):
    """Pin the branch and prove its sources occur in main before deleting a squash branch."""
    head = subprocess.check_output(["git", "rev-parse", "--verify", f"refs/heads/{branch}"], cwd=cwd).strip().decode()
    main = subprocess.check_output(["git", "rev-parse", "--verify", "refs/heads/main"], cwd=cwd).strip().decode()
    if subprocess.run(["git", "merge-base", "--is-ancestor", head, main], cwd=cwd).returncode == 0:
        return head
    tree = subprocess.check_output(["git", "rev-parse", f"{head}^{{tree}}"], cwd=cwd).strip().decode()
    trees = subprocess.check_output(["git", "log", "--first-parent", "--format=%T", main], cwd=cwd).decode().splitlines()
    # Squash merges change commit identity. Compare the whole tree, not selected files or
    # individual patches; later main commits may have changed those sources again.
    if tree in trees:
        return head
    raise ValueError("worktree has commits not in main; squash-merge its complete sources before removal")


def _remove(cwd, name):
    common = common_dir(cwd)
    if common is None:
        raise ValueError("worktrees require a Git repository")
    path, registry = _registry(common)
    if name not in registry:
        raise ValueError("only worktrees created by this tool can be removed")
    item = registry[name]
    destination = Path(item["path"])
    branch = subprocess.check_output(["git", "branch", "--show-current"], cwd=destination).strip().decode()
    if branch != item["branch"]:
        raise ValueError("worktree is on a different branch; preserve it before removal")
    if subprocess.check_output(["git", "status", "--porcelain"], cwd=destination).strip():
        raise ValueError("worktree has local changes; preserve or commit them before removal")
    ignored = subprocess.check_output(["git", "ls-files", "--others", "--ignored", "--exclude-standard", "-z"], cwd=destination)
    for ignored_name in ignored.split(b"\0"):
        if not ignored_name:
            continue
        relative = Path(os.fsdecode(ignored_name))
        if "node_modules" in relative.parts:
            continue
        if len(relative.parts) > 1 and relative.parts[0] == "target" and relative.parts[1].startswith("test-scratch-"):
            continue
        raise ValueError(f"preserve ignored output before removal: {relative}")
    head = merged_head(cwd, item["branch"])
    from jobs import entries, locked
    from resources import resource_dir
    resources = resource_dir()
    with locked(resources):
        if any(Path(job["cwd"]).resolve().is_relative_to(destination.resolve()) for job in entries(resources)):
            raise ValueError("worktree has queued or running jobs")
        if subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=destination).strip().decode() != head:
            raise ValueError("worktree branch changed during removal; preserve it and retry")
        subprocess.run(["git", "worktree", "remove", str(destination)], cwd=cwd, check=True)
        registry.pop(name)
        _save(path, registry)
        # A squash branch is not an ancestor, so `branch -d` cannot prove it merged. The
        # tree proof above permits deletion; the expected old ref preserves a raced commit.
        subprocess.run(["git", "update-ref", "-d", f"refs/heads/{item['branch']}", head], cwd=cwd, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="action", required=True)
    creation = sub.add_parser("create")
    creation.add_argument("name")
    creation.add_argument("--ref", default="main")
    removal = sub.add_parser("remove")
    removal.add_argument("name")
    cleanup = sub.add_parser("prune")
    cleanup.add_argument("--build-cache", action="store_true")
    sub.add_parser("status")
    sub.add_parser("env")
    args = parser.parse_args()
    cwd = os.getcwd()
    if args.action == "env":
        for key, value in environment(cwd).items():
            print(f"export {key}={shlex.quote(value)}")
    elif args.action == "create":
        print(create(cwd, args.name, args.ref))
    elif args.action == "remove":
        remove(cwd, args.name)
    elif args.action == "prune":
        from jobs import admitted, run
        if not admitted():
            return run("background", [sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]], maintenance=False)
        maintain(cwd, args.build_cache)
    else:
        common = common_dir(cwd)
        if common:
            print(f"shared caches: {allocated(common / 'wrela/cache') / GIB:.2f} GiB / 20 GiB")
            worktrees = subprocess.check_output(["git", "worktree", "list", "--porcelain"], cwd=cwd).decode()
            for line in worktrees.splitlines():
                if line.startswith("worktree "):
                    root = Path(line[9:])
                    print(f"{root}: local target {allocated(root / 'target') / GIB:.2f} GiB")
        print(f"free: {shutil.disk_usage(cwd).free / GIB:.1f} GiB (12 GiB admission reserve)")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"worktrees: {error}")
