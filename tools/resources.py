#!/usr/bin/env python3
"""CPU workers available to a gate, including the quota of its cloud container."""

import os
from pathlib import Path
import re
import subprocess
import sys
import time


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


def _cgroup_paths(wanted):
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
            controller = "" if kind == "cgroup2" else wanted
            if kind != "cgroup2" and (kind != "cgroup" or wanted not in options.split(",")):
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
            yield kind, current
            if current == mount:
                break
            current = current.parent


def _quota_counts():
    for kind, current in _cgroup_paths("cpu"):
        try:
            if kind == "cgroup2":
                quota, period = _read(current / "cpu.max").split()
            else:
                quota = _read(current / "cpu.cfs_quota_us")
                period = _read(current / "cpu.cfs_period_us")
            quota, period = int(quota), int(period)
            if quota > 0 and period > 0:
                yield max(1, quota // period)
        except ValueError:
            pass


def _memory_quota_pressure():
    pressure = 0
    for kind, current in _cgroup_paths("memory"):
        try:
            usage_file, limit_file = ("memory.current", "memory.max") if kind == "cgroup2" else (
                "memory.usage_in_bytes", "memory.limit_in_bytes")
            used = int(_read(current / usage_file))
            stats = dict(re.findall(r"^(\w+)\s+(\d+)", _read(current / "memory.stat"), re.M))
            inactive = stats.get("inactive_file" if kind == "cgroup2" else "total_inactive_file", "0")
            working = max(0, used - int(inactive))
            limit = _read(current / limit_file).strip()
            if limit != "max" and int(limit) > 0:
                ratio = working / int(limit)
                pressure = max(pressure, 2 if ratio >= .95 else 1 if ratio >= .9 else 0)
            if kind == "cgroup2":
                high = _read(current / "memory.high").strip()
                if high not in ("", "max") and int(high) > 0 and working >= int(high):
                    pressure = max(pressure, 1)
        except ValueError:
            pass
    return pressure


def cpu_count():
    """Return at least one worker, bounded by affinity and all visible cgroup quotas."""
    counts = [os.cpu_count() or 1]
    try:
        counts.append(len(os.sched_getaffinity(0)))
    except (AttributeError, OSError):
        pass  # platforms without Linux affinity or cgroups
    counts.extend(_quota_counts())
    return max(1, min(counts))


def worker_count():
    """The admitted job's CPU allowance, distinct from the machine's capacity."""
    try:
        return max(1, min(cpu_count(), int(os.environ.get("WRELA_CPU_WORKERS", "2"))))
    except ValueError:
        return min(2, cpu_count())


def resource_dir(env=None):
    """One per-user location across checkouts; TMPDIR never selects production permits."""
    env = os.environ if env is None else env
    root = Path(env.get("WRELA_RESOURCE_DIR") or Path.home() / ".cache/wrela/resources")
    if not root.is_absolute():
        raise ValueError("WRELA_RESOURCE_DIR must be absolute")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    return root


def memory_pressure():
    """Normal (0), warning (1), critical (2); unavailable telemetry doesn't stop work."""
    if sys.platform == "darwin":
        try:
            result = subprocess.run(["sysctl", "-n", "kern.memorystatus_vm_pressure_level"],
                                    capture_output=True, text=True, timeout=2)
            level = int(result.stdout)
            return 2 if level & 4 else 1 if level & 2 else 0
        except (OSError, ValueError, subprocess.TimeoutExpired):
            return 0
    values = dict(re.findall(r"^(\w+):\s+(\d+)", _read(Path("/proc/meminfo")), re.M))
    total, available = int(values.get("MemTotal", 0)), int(values.get("MemAvailable", 0))
    host = (2 if available < total * .05 else 1 if available < total * .1 else 0) if total else 0
    return max(host, _memory_quota_pressure())


def gpu_locks(shared):
    """Descriptors for the global barrier and, for a small check, one of two GPU slots.

    Callers pass these to their renderer process, so a killed wrapper cannot free a live
    renderer's permit. Native Rust follows this exact protocol too.
    """
    import fcntl
    root = resource_dir()
    descriptors = []
    try:
        barrier = os.open(root / "gpu.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        descriptors.append(barrier)
        fcntl.flock(barrier, fcntl.LOCK_SH if shared else fcntl.LOCK_EX)
        if shared:
            slots = []
            try:
                for n in range(2):
                    slots.append(os.open(root / f"gpu-slot-{n}.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600))
                while True:
                    for slot in slots:
                        try:
                            fcntl.flock(slot, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            descriptors.append(slot)
                            slots.remove(slot)
                            return descriptors
                        except BlockingIOError:
                            pass
                    time.sleep(.05)
            finally:
                for slot in slots:
                    os.close(slot)
        return descriptors
    except BaseException:
        for descriptor in descriptors:
            os.close(descriptor)
        raise


if __name__ == "__main__":
    print(cpu_count())
