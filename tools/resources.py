#!/usr/bin/env python3
"""CPU workers available to a gate, including the quota of its cloud container."""

import os
from pathlib import Path
import re


_CGROUPS = Path("/proc/self/cgroup")
_MOUNTS = Path("/proc/self/mountinfo")


def _read(path):
    try:
        return path.read_text()
    except OSError:
        return ""


def _unescape(path):
    # mountinfo escapes spaces, tabs, newlines and backslashes as octal.
    return re.sub(r"\\([0-7]{3})", lambda m: chr(int(m[1], 8)), path)


def _quota_counts():
    groups = {}
    for line in _read(_CGROUPS).splitlines():
        fields = line.split(":", 2)
        if len(fields) == 3:
            for controller in fields[1].split(","):
                groups[controller] = Path(fields[2])
    for line in _read(_MOUNTS).splitlines():
        fields = line.split()
        try:
            separator = fields.index("-")
            kind, options = fields[separator + 1], fields[separator + 3]
            controller = "" if kind == "cgroup2" else "cpu"
            if kind != "cgroup2" and (kind != "cgroup" or "cpu" not in options.split(",")):
                continue
            group = groups[controller]
            root, mount = Path(_unescape(fields[3])), Path(_unescape(fields[4]))
        except (ValueError, IndexError, KeyError):
            continue
        try:
            relative = group.relative_to(root)
        except ValueError:
            # A cgroup namespace reports its paths relative to its own root.
            relative = group.relative_to("/")
        if ".." in relative.parts:
            continue
        current = mount / relative
        while True:
            try:
                if kind == "cgroup2":
                    quota, period = _read(current / "cpu.max").split()
                else:
                    quota = _read(current / "cpu.cfs_quota_us")
                    period = _read(current / "cpu.cfs_period_us")
                quota, period = int(quota), int(period)
                if quota > 0 and period > 0:
                    # Whole CPU workers avoid oversubscribing CPU-bound builds.
                    yield max(1, quota // period)
            except ValueError:
                pass  # unlimited ("max" or -1), unavailable, or malformed
            if current == mount:
                break
            current = current.parent


def cpu_count():
    """Return at least one worker, bounded by affinity and all visible cgroup quotas."""
    counts = [os.cpu_count() or 1]
    try:
        counts.append(len(os.sched_getaffinity(0)))
    except (AttributeError, OSError):
        pass  # platforms without Linux affinity or cgroups
    counts.extend(_quota_counts())
    return max(1, min(counts))


if __name__ == "__main__":
    print(cpu_count())
