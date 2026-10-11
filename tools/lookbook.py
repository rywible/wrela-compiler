#!/usr/bin/env python3
"""The floor's lookbook (#54 §9, #55 AC10): each shot of examples/last-green/lookbook.wrela
rendered settled at 1080p, as a still on the native host and as a 10 s clip in Chrome (its move),
into a directory.

    tools/lookbook.py <out-dir> [--stills] [--capture [--warm]] [--view clay|silhouette]
                      [--size WxH] [--jobs N] [shot-number ...]

The floor must be built (`wrela build examples/last-green`). A shot is chosen as a player would
(L, its number, Enter); the still is the frame its move starts at (`lookbook::SETTLE` seconds in),
when the tiles round it have streamed in and its caches have settled; the clip is its move, from
then for `lookbook::MOVE` seconds, paced as in play, recorded from Chrome's canvas (test mode's
`clip`: a WebM). `--stills` makes the stills alone. `--view` draws a debug view of form instead of
the look (F8 in the floor): `clay`, the scene's light on its forms all one matte grey, or
`silhouette`, the sky white, the land light and the solid forms dark, a step lighter for each band
of distance. `--size` renders the stills at another size than 1920x1080 (1280x720 while
iterating). `--jobs` renders that many stills at once (by default 3 on a machine whose GPU is
software, else 1), each process sharing the GPU lock (`WRELA_GPU_SHARED`), with the same pixels:
on a 4-core cloud machine lavapipe leaves cores idle in a still's load frames, and shots 2, 7
and 9 took 785 s at once, about 1400 s one by one. Each shot's numbers, the
interest check's view at its eye (the build's `files/check/shots.json`), are printed beside it and saved as
`check.json`.

`--capture` takes each still in capture mode (F7, `Floor::capture` in examples/last-green/main.wrela):
the floor does its load in a few frames, and the still is the frame it prints `still at frame N` on,
a 16-frame resolve once the light round the eye is in, not the frame 5 s in (shot 2 at 1080p on a
4-core cloud machine: 136 s, not 460 s; 37 s of it is the host's start, lavapipe compiling the
pipelines). With `--warm`, one process takes every shot's still in turn, so the floor loads once:
after a shot's still, the next shot is chosen (shots 2, 7 and 9: 176 s, about 20 s a shot after
the first). A warm shot can differ from the same shot taken cold (shot 7 after shot 2: the far
shore lit, not shaded, 8/255 apart on average), as after a travel in play: take a baseline cold.
"""

import json, os, re, shutil, subprocess, sys, tempfile
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BUILD = os.path.join(ROOT, "examples/last-green/build")
HOST = os.path.join(ROOT, "target/release/wrela-host")
HEADLESS = os.path.join(ROOT, "tools/headless.py")
SETTLE_FRAMES = 300
# A capture's frames at most, its load and its resolve: it stops before, at `STILL`.
CAPTURE_FRAMES = 900
STILL = "still at frame"
MOVE_FRAMES = 600


def names():
    text = open(os.path.join(ROOT, "examples/last-green/lookbook.wrela")).read()
    return re.findall(r'Shot \{\s*name: "([^"]+)"', text)


# How many presses of F8 draw each debug view of form (`view` in examples/last-green/main.wrela).
VIEWS = {"look": 0, "clay": 1, "silhouette": 2}


def keys(i, view="look"):
    """The keys that choose shot `i` (L, its number, Enter), after F8 for the debug `view`."""
    events = [{"frame": 1, "type": "key", "key": "F8"}] * VIEWS[view]
    events += [{"frame": 1, "type": "key", "key": "KeyL"}]
    events += [{"frame": 1, "type": "key", "key": "Digit" + d} for d in str(i)]
    events.append({"frame": 1, "type": "key", "key": "Enter"})
    return events


def stem(i, name):
    return f"{i:02d}-{name.replace(' ', '-').replace(chr(39), '')}"


def capture_keys(i, view="look", first=True):
    """The keys that take shot `i`'s still in capture mode: F7 first (it toggles), then the shot."""
    f7 = [{"frame": 1, "type": "key", "key": "F7"}] if first else []
    return f7 + keys(i, view if first else "look")


def run(scripts, png, size, capture, env=None):
    """The host on `scripts` (each a list of events), its PNG at `png`: the frame 5 s in, or
    with `capture`, each script's frame that prints `STILL`."""
    tmp = tempfile.mkdtemp(prefix="lookbook-")
    cmd = [HOST, BUILD, "--size", size, "--png", png]
    for k, events in enumerate(scripts):
        path = os.path.join(tmp, f"keys-{k}.json")
        with open(path, "w") as f:
            json.dump(events, f)
        cmd += ["--input", path]
    frames = CAPTURE_FRAMES * len(scripts) if capture else SETTLE_FRAMES + 1
    cmd += ["--frames", str(frames)] + (["--until", STILL] if capture else [])
    r = subprocess.run(cmd, capture_output=True, text=True, env=env)
    shutil.rmtree(tmp)
    return r


def still(i, name, out, view="look", size="1920x1080", jobs=1, capture=False):
    png = os.path.join(out, stem(i, name) + ".png")
    env = dict(os.environ, WRELA_GPU_SHARED=str(max(2, jobs))) if jobs > 1 else None
    events = capture_keys(i, view) if capture else keys(i, view)
    r = run([events], png, size, capture, env)
    if r.returncode != 0:
        sys.exit(f"shot {i} ({name}) failed:\n{r.stderr[-2000:]}")
    return png


def warm(pick, shots, out, view="look", size="1920x1080"):
    """The capture stills of the shots `pick` from one process, in turn: the floor loads once."""
    tmp = tempfile.mkdtemp(prefix="lookbook-")
    scripts = [capture_keys(i, view, first=k == 0) for k, i in enumerate(pick)]
    r = run(scripts, os.path.join(tmp, "w.png"), size, True)
    if r.returncode != 0:
        sys.exit(f"the warm run failed:\n{r.stderr[-2000:]}")
    pngs = []
    for k, i in enumerate(pick):
        png = os.path.join(out, stem(i, shots[i]) + ".png")
        shutil.move(os.path.join(tmp, "w.png" if len(pick) == 1 else f"w-{k:04d}.png"), png)
        pngs.append(png)
    shutil.rmtree(tmp)
    return pngs


def clip(i, name, out, view="look"):
    """Shot `i`'s move as a 10 s clip in Chrome (1080p, paced, eight threads), into `out`."""
    script = os.path.join(BUILD, "lookbook-keys.json")
    with open(script, "w") as f:
        json.dump(keys(i, view), f)
    frames = SETTLE_FRAMES + MOVE_FRAMES + 30
    hash = (f"#test&frames={frames}&width=1920&height=1080&fps=60&workers=8&paced=1&nohash=1"
            f"&input=lookbook-keys.json&clipfrom={SETTLE_FRAMES}&clip={MOVE_FRAMES}")
    r = subprocess.run([sys.executable, HEADLESS, os.path.relpath(BUILD, ROOT), hash, "600"],
                       capture_output=True, text=True)
    os.unlink(script)
    if r.returncode != 0:
        sys.exit(f"shot {i} ({name})'s clip failed:\n{r.stderr[-2000:]}")
    webm = os.path.join(out, stem(i, name) + ".webm")
    shutil.copyfile(os.path.join(BUILD, "results", "clip.webm"), webm)
    return webm


def main():
    args = sys.argv[1:]
    if not args:
        sys.exit(__doc__)
    stills_only = "--stills" in args
    capture = "--capture" in args
    warm_run = "--warm" in args
    if warm_run and not capture:
        sys.exit("--warm takes --capture")
    args = [a for a in args if a not in ("--stills", "--capture", "--warm")]
    view = "look"
    if "--view" in args:
        k = args.index("--view")
        if k + 1 >= len(args) or args[k + 1] not in VIEWS:
            sys.exit(f"--view takes one of {', '.join(VIEWS)}")
        view = args[k + 1]
        del args[k:k + 2]
    size = "1920x1080"
    if "--size" in args:
        k = args.index("--size")
        if k + 1 >= len(args) or not re.fullmatch(r"\d+x\d+", args[k + 1]):
            sys.exit("--size takes WxH, as 1280x720")
        size = args[k + 1]
        del args[k:k + 2]
    software = os.environ.get("WRELA_SOFTWARE_GPU") == "1"
    jobs = 3 if software else 1
    if "--jobs" in args:
        k = args.index("--jobs")
        if k + 1 >= len(args) or not args[k + 1].isdigit() or int(args[k + 1]) < 1:
            sys.exit("--jobs takes a whole number, 1 or more")
        jobs = int(args[k + 1])
        del args[k:k + 2]
    out = args[0]
    os.makedirs(out, exist_ok=True)
    shots = names()
    pick = [int(a) for a in args[1:]] or range(len(shots))
    numbers = os.path.join(BUILD, "files", "check", "shots.json")
    views = json.load(open(numbers)) if os.path.exists(numbers) else []
    if views:
        shutil.copyfile(numbers, os.path.join(out, "check.json"))
    # The stills at once, `jobs` at a time (each printed, in order, as it and those before it end);
    # then each clip in turn, as Chrome takes the GPU alone.
    if warm_run:
        pngs = warm(pick, shots, out, view, size)
    else:
        pool = ThreadPoolExecutor(jobs)
        runs = [pool.submit(still, i, shots[i], out, view, size, jobs, capture) for i in pick]
        pngs = (r.result() for r in runs)
    for i, png in zip(pick, pngs):
        print(png, flush=True)
        if i < len(views):
            v = views[i]["view"]
            what = ", ".join(f"{d['pull']} {d['strength']:.2f}" for d in v["draws"][:4])
            state = "dead" if v["dead"] else "quiet" if v["quiet"] else f"{v['count']} draw"
            print(f"  the check: {state}; strongest {v['best']:.2f}" + (f" ({what})" if what else ""))
    if not stills_only:
        for i in pick:
            print(clip(i, shots[i], out, view))


if __name__ == "__main__":
    main()
