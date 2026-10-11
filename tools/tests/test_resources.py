"""CPU worker limits must follow the container's quota, not the host's CPU count."""

import pathlib
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
import resources  # noqa: E402


class CpuCount(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.groups = self.root / "cgroup"
        self.mounts = self.root / "mountinfo"
        self.mount = self.root / "cpu mount"
        self.mount.mkdir()
        for name, value in (("_CGROUPS", self.groups), ("_MOUNTS", self.mounts)):
            patcher = patch.object(resources, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)

    def hierarchy(self, group="/", mount_root="/", kind="cgroup2"):
        self.groups.write_text(f"0::{group}\n" if kind == "cgroup2" else f"4:cpu,cpuacct:{group}\n")
        escaped = str(self.mount).replace(" ", r"\040")
        options = "rw" if kind == "cgroup2" else "rw,cpu,cpuacct"
        self.mounts.write_text(f"29 23 0:26 {mount_root} {escaped} rw - {kind} cgroup {options}\n")

    def quota(self, group, value, kind="cgroup2"):
        directory = self.mount / group
        directory.mkdir(parents=True, exist_ok=True)
        if kind == "cgroup2":
            (directory / "cpu.max").write_text(value)
        else:
            quota, period = value.split()
            (directory / "cpu.cfs_quota_us").write_text(quota)
            (directory / "cpu.cfs_period_us").write_text(period)

    def count(self, host=64, affinity=32):
        with patch.object(resources.os, "cpu_count", return_value=host), patch.object(
            resources.os, "sched_getaffinity", return_value=set(range(affinity)), create=True
        ):
            return resources.cpu_count()

    def test_container_quota_bounds_a_large_host(self):
        self.hierarchy()
        self.quota("", "200000 100000")
        self.assertEqual(self.count(), 2)

    def test_parent_quota_bounds_a_nested_unlimited_group(self):
        self.hierarchy("/team/task")
        self.quota("", "600000 100000")
        self.quota("team", "200000 100000")
        self.quota("team/task", "max 100000")
        self.assertEqual(self.count(), 2)

    def test_child_quota_can_be_stricter_than_parent(self):
        self.hierarchy("/team/task")
        self.quota("", "800000 100000")
        self.quota("team/task", "300000 100000")
        self.assertEqual(self.count(), 3)

    def test_mount_root_is_removed_from_absolute_membership(self):
        self.hierarchy("/tenant/task", mount_root="/tenant")
        self.quota("task", "200000 100000")
        self.assertEqual(self.count(), 2)

    def test_namespace_membership_is_relative_to_mount(self):
        self.hierarchy("/task", mount_root="/tenant")
        self.quota("task", "200000 100000")
        self.assertEqual(self.count(), 2)

    def test_affinity_and_host_still_bound_workers(self):
        self.hierarchy()
        self.quota("", "800000 100000")
        self.assertEqual(self.count(affinity=3), 3)
        self.assertEqual(self.count(host=2), 2)

    def test_fractional_quota_uses_whole_workers_with_at_least_one(self):
        self.hierarchy()
        self.quota("", "190000 100000")
        self.assertEqual(self.count(), 1)
        self.quota("", "20000 100000")
        self.assertEqual(self.count(), 1)

    def test_v1_parent_quota_is_respected(self):
        self.hierarchy("/team/task", kind="cgroup")
        self.quota("team", "200000 100000", kind="cgroup")
        self.quota("team/task", "-1 100000", kind="cgroup")
        self.assertEqual(self.count(), 2)

    def test_missing_or_malformed_cgroups_fall_back_to_affinity(self):
        self.assertEqual(self.count(), 32)
        self.hierarchy()
        for value in ("", "max 100000", "broken", "200000 0", "-1 100000"):
            self.quota("", value)
            self.assertEqual(self.count(), 32, value)

    def test_missing_platform_facilities_still_give_one_worker(self):
        with patch.object(resources.os, "cpu_count", return_value=None), patch.object(
            resources.os, "sched_getaffinity", side_effect=AttributeError, create=True
        ):
            self.assertEqual(resources.cpu_count(), 1)


if __name__ == "__main__":
    unittest.main()
