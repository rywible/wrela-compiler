"""Capture tools preserve frame history while budgeting work for the available CPUs."""

import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from urllib.parse import parse_qs

TOOLS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, TOOLS)

import lookbook  # noqa: E402
import motion  # noqa: E402
import round as rounds  # noqa: E402


class MotionCpuTest(unittest.TestCase):
    def test_only_kept_frames_are_saved_and_frame_numbers_survive(self):
        events = [motion.key(4, "KeyP"), motion.key(1, "KeyL")]
        calls, scripts = [], []

        def run(args, **kwargs):
            calls.append(args)
            if args[0] == motion.HOST:
                script = Path(args[args.index("--input") + 1])
                scripts.append(script)
                self.assertEqual(json.loads(script.read_text()), list(reversed(events)))
                self.assertEqual(args[args.index("--frames") + 1], "9")
                self.assertEqual(args[args.index("--png-from") + 1], "3")
                destination = Path(args[args.index("--png") + 1]).parent
                for frame in range(3, 9):
                    (destination / f"f-{frame:04d}.png").write_bytes(b"frame")
            return subprocess.CompletedProcess(args, 0, "", "")

        with tempfile.TemporaryDirectory() as out, \
             mock.patch.dict(motion.CAPTURES, {"small": (events, 3, 8)}), \
             mock.patch.object(motion.subprocess, "run", side_effect=run), \
             mock.patch.object(motion, "cpu_count", return_value=2):
            self.assertEqual(motion.capture("small", out, "64x64"), os.path.join(out, "small.mp4"))
            self.assertEqual(sorted(os.listdir(os.path.join(out, "small"))),
                             [f"{frame:05d}.png" for frame in range(3, 9)])
        self.assertTrue(all(not script.exists() for script in scripts))
        self.assertEqual(len(calls), 3)
        video, sheet = calls[1:]
        self.assertEqual(video[video.index("-start_number") + 1], "3")
        self.assertEqual(video[video.index("-c:v") + 1], "libx264")
        self.assertEqual(video[-7:-5], ["-threads", "2"])
        self.assertEqual(sheet[sheet.index("-filter_complex_threads") + 1], "2")
        self.assertEqual([Path(sheet[i + 1]).name for i, arg in enumerate(sheet) if arg == "-i"],
                         [f"{frame:05d}.png" for frame in range(3, 9)])

    def test_host_start_failure_removes_input_script(self):
        scripts = []

        def fail(args, **kwargs):
            scripts.append(Path(args[args.index("--input") + 1]))
            raise OSError("host unavailable")

        with tempfile.TemporaryDirectory() as out, \
             mock.patch.object(motion.subprocess, "run", side_effect=fail), \
             self.assertRaisesRegex(OSError, "host unavailable"):
            motion.capture("walk", out, "64x64")
        self.assertTrue(all(not script.exists() for script in scripts))


class LookbookCpuTest(unittest.TestCase):
    def test_default_jobs_follow_quota_and_requested_shot_count(self):
        cases = [(1, True, [], 1), (2, True, [], 1), (4, True, [], 1),
                 (16, True, [], 1), (16, False, [], 1), (4, True, ["0"], 1),
                 (4, True, ["--jobs", "4"], 2)]
        for cores, software, options, jobs in cases:
            with self.subTest(cores=cores, software=software, options=options), \
                 tempfile.TemporaryDirectory() as out, \
                 mock.patch.object(sys, "argv", ["lookbook.py", out, "--stills", *options]), \
                 mock.patch.dict(os.environ, {"WRELA_SOFTWARE_GPU": "1" if software else "0"}), \
                 mock.patch.object(lookbook, "BUILD", out), \
                 mock.patch.object(lookbook, "cpu_count", return_value=cores), \
                 mock.patch.object(lookbook, "names", return_value=["shot"] * 6), \
                 mock.patch.object(lookbook, "still", return_value="shot.png") as still, \
                 contextlib.redirect_stdout(io.StringIO()):
                lookbook.main()
                self.assertTrue(still.called)
                self.assertEqual({call.args[5] for call in still.call_args_list}, {jobs})

    def test_software_shots_bound_mesa_by_each_shots_cpu_budget(self):
        for cores, jobs, override, expected in [(2, 2, None, 1), (4, 1, None, 4),
                                                (16, 3, None, 5), (16, 3, "7", 5)]:
            with self.subTest(cores=cores, jobs=jobs, override=override), \
                 tempfile.TemporaryDirectory() as out, \
                 mock.patch.dict(os.environ, {"WRELA_SOFTWARE_GPU": "1"}, clear=True), \
                 mock.patch.object(lookbook, "cpu_count", return_value=cores), \
                 mock.patch.object(lookbook.subprocess, "run", return_value=
                                   subprocess.CompletedProcess([], 0, "", "")) as run:
                if override:
                    os.environ["LP_NUM_THREADS"] = override
                lookbook.still(0, "shot", out, jobs=jobs, capture=True)
                args = run.call_args.args[0]
                env = run.call_args.kwargs["env"]
                self.assertEqual(args[args.index("--workers") + 1], str(expected))
                self.assertEqual(env["LP_NUM_THREADS"], str(expected))
                self.assertEqual(args[args.index("--until") + 1], lookbook.STILL)
                self.assertFalse(Path(args[args.index("--input") + 1]).exists())

    def test_browser_workers_follow_quota(self):
        for cores in (1, 2, 4, 16):
            with self.subTest(cores=cores), tempfile.TemporaryDirectory() as out, \
                 mock.patch.object(lookbook, "BUILD", out), \
                 mock.patch.object(lookbook, "cpu_count", return_value=cores), \
                 mock.patch.object(lookbook.subprocess, "run", return_value=
                                   subprocess.CompletedProcess([], 0, "", "")) as run:
                Path(out, "results").mkdir()
                Path(out, "results", "clip.webm").write_bytes(b"clip")
                lookbook.clip(0, "shot", out)
                params = parse_qs(run.call_args.args[0][3].split("&", 1)[1])
                self.assertEqual(params["workers"], [str(min(8, cores))])
                self.assertEqual(params["clipfrom"], [str(lookbook.SETTLE_FRAMES)])
                self.assertEqual(params["clip"], [str(lookbook.MOVE_FRAMES)])


class RoundCpuTest(unittest.TestCase):
    def test_short_round_sizes_browser_workers_to_cpu_quota(self):
        for cores in (2, 4, 16):
            with self.subTest(cores=cores), tempfile.TemporaryDirectory() as root:
                build = Path(root, "build")
                build.mkdir()

                def run(args, **kwargs):
                    params = parse_qs(args[3].split("&", 1)[1])
                    self.assertEqual(params["workers"], [str(min(8, cores))])
                    self.assertEqual(params["frames"], ["2"])
                    results = Path(root, args[2], "results")
                    results.mkdir()
                    (results / "frames.json").write_text('{"began_ms": [0, 16], "printed_in": []}')
                    (results / "log.txt").write_text("")
                    return subprocess.CompletedProcess(args, 0, "", "")

                with mock.patch.object(rounds, "ROOT", root), \
                     mock.patch.object(rounds, "BUILD", str(build)), \
                     mock.patch.object(rounds, "cpu_count", return_value=cores), \
                     mock.patch.object(rounds, "script", return_value=([], 60)), \
                     mock.patch.object(rounds.subprocess, "run", side_effect=run), \
                     mock.patch.object(sys, "argv", ["round.py", "0", "--no-lookbook", "--frames", "2"]), \
                     contextlib.redirect_stdout(io.StringIO()), self.assertRaises(SystemExit) as exit:
                    rounds.main()
                self.assertEqual(exit.exception.code, 0)


if __name__ == "__main__":
    unittest.main()
