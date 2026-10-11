#!/usr/bin/env python3
"""The floor's lookbook (#54 §9, #55 AC10): each shot of examples/last-green/lookbook.wrela
rendered settled at 1080p, as a still on the native host and as a 10 s clip in Chrome (its move),
into a directory.

    tools/lookbook.py <out-dir> [--stills] [--view clay|silhouette] [--size WxH] [shot-number ...]

The floor must be built (`wrela build examples/last-green`). A shot is chosen as a player would
(L, its number, Enter); the still is the frame its move starts at (`lookbook::SETTLE` seconds in),
when the tiles round it have streamed in and its caches have settled; the clip is its move, from
then for `lookbook::MOVE` seconds, paced as in play, recorded from Chrome's canvas (test mode's
`clip`: a WebM). `--stills` makes the stills alone. `--view` draws a debug view of form instead of
the look (F8 in the floor): `clay`, the scene's light on its forms all one matte grey, or
`silhouette`, the sky white, the land light and the solid forms dark, a step lighter for each band
of distance. `--size` renders the stills at another size than 1920x1080 (1280x720 while
iterating). Each shot's numbers, the interest check's
view at its eye (the build's `files/check/shots.json`), are printed beside it and saved as
`check.json`.
"""

import json, os, re, shutil, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BUILD = os.path.join(ROOT, "examples/last-green/build")
HOST = os.path.join(ROOT, "target/release/wrela-host")
HEADLESS = os.path.join(ROOT, "tools/headless.py")
SETTLE_FRAMES = 300
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


def still(i, name, out, view="look", size="1920x1080"):
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        json.dump(keys(i, view), f)
        script = f.name
    png = os.path.join(out, stem(i, name) + ".png")
    r = subprocess.run([HOST, BUILD, "--frames", str(SETTLE_FRAMES + 1), "--size", size,
                        "--input", script, "--png", png], capture_output=True, text=True)
    os.unlink(script)
    if r.returncode != 0:
        sys.exit(f"shot {i} ({name}) failed:\n{r.stderr[-2000:]}")
    return png


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
    args = [a for a in args if a != "--stills"]
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
    out = args[0]
    os.makedirs(out, exist_ok=True)
    shots = names()
    pick = [int(a) for a in args[1:]] or range(len(shots))
    numbers = os.path.join(BUILD, "files", "check", "shots.json")
    views = json.load(open(numbers)) if os.path.exists(numbers) else []
    if views:
        shutil.copyfile(numbers, os.path.join(out, "check.json"))
    for i in pick:
        print(still(i, shots[i], out, view, size))
        if i < len(views):
            v = views[i]["view"]
            what = ", ".join(f"{d['pull']} {d['strength']:.2f}" for d in v["draws"][:4])
            state = "dead" if v["dead"] else "quiet" if v["quiet"] else f"{v['count']} draw"
            print(f"  the check: {state}; strongest {v['best']:.2f}" + (f" ({what})" if what else ""))
        if not stills_only:
            print(clip(i, shots[i], out, view))


if __name__ == "__main__":
    main()
