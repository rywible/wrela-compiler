#!/usr/bin/env python3
"""The floor in motion: short, repeatable captures of what a still can't show (the temporal AA's
history, the wind in the leaves, arriving somewhere), each a run of frames on the native host
saved as they're drawn (`wrela-host --png --every`), into a directory.

    tools/motion.py <out-dir> [--size WxH] [capture ...]

The floor must be built (`wrela build examples/last-green`). Each capture is a script of the
player's keys (a lookbook shot chosen as a player would: L, its number, Enter) and the frames it
keeps, at 60 a second:

- walk: the floor's walk at a run (P), from the gate, 4 s of it.
- reversal: a run from the gate, the camera turned hard left then hard right (the arrow keys
  held, 1.2 s each way).
- arrive-dark: the clearing's shot held 5 s, then the beech wood's: its first 2.5 s (black
  until the light there is drawn, then its first second and more).
- arrive-sun: the beech wood's shot held 5 s, then the clearing's: the same.
- gust: the wood pasture's shot held, the wind in its crowns: 4 s, the view settled.
- canopy: the clearing's move, crowns against the sky: 2 s.
- water: the mere's move: 2 s.
- occlusion: the move of the fawn under the beeches, the stand-in and the fawn passing behind
  trunks and ferns: 2 s.

Each capture is kept as its frames (`<name>/NNNNN.png`, the frame's number), a clip
(`<name>.mp4`, 60 fps, near-lossless) and a contact sheet of six frames (`<name>-sheet.jpg`).
1280x720 unless `--size` says otherwise. With no capture named, all of them, one at a time.
"""

import json, os, shutil, subprocess, sys, tempfile

from resources import cpu_count

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BUILD = os.path.join(ROOT, "examples/last-green/build")
HOST = os.path.join(ROOT, "target/release/wrela-host")
# A lookbook shot's move starts this many frames after it's chosen (tools/lookbook.py).
SETTLE = 300


def key(frame, k, kind="key"):
    return {"frame": frame, "type": kind, "key": k}


def shot(frame, i):
    """The keys that choose lookbook shot `i` at `frame`."""
    return [key(frame, "KeyL")] + [key(frame, "Digit" + d) for d in str(i)] + [key(frame, "Enter")]


def held(k, down, up):
    return [key(down, k, "keydown"), key(up, k, "keyup")]


# Each capture: its input events, and the first and last frames it keeps.
CAPTURES = {
    "walk": ([key(1, "KeyP")], 240, 480),
    "reversal": (held("KeyW", 200, 460) + held("ShiftLeft", 200, 460) + held("ArrowLeft", 300, 372)
                 + held("ArrowRight", 372, 444), 285, 460),
    "arrive-dark": (shot(1, 0) + shot(SETTLE, 2), SETTLE, SETTLE + 150),
    "arrive-sun": (shot(1, 2) + shot(SETTLE, 0), SETTLE, SETTLE + 150),
    "gust": (shot(1, 1), 60, 300),
    "canopy": (shot(1, 0), SETTLE, SETTLE + 120),
    "water": (shot(1, 7), SETTLE, SETTLE + 120),
    "occlusion": (shot(1, 17), SETTLE, SETTLE + 120),
}


def capture(name, out, size):
    events, first, last = CAPTURES[name]
    frames = os.path.join(out, name)
    shutil.rmtree(frames, ignore_errors=True)
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        # (The host takes events in frame order.)
        json.dump(sorted(events, key=lambda e: e["frame"]), f)
        script = f.name
    os.makedirs(frames)
    try:
        r = subprocess.run([HOST, BUILD, "--frames", str(last + 1), "--size", size, "--input", script,
                            "--png", os.path.join(frames, "f.png"), "--every", "1",
                            "--png-from", str(first)], capture_output=True, text=True)
    finally:
        os.unlink(script)
    if r.returncode != 0:
        sys.exit(f"capture {name} failed:\n{r.stderr[-2000:]}")
    # Pre-roll still renders so temporal history is unchanged; only saved frames cross back
    # to the CPU and get encoded. The host keeps its original frame numbers (f-NNNN.png).
    for f in os.listdir(frames):
        if f.startswith("f-"):
            i = int(f[2:-4])
            path = os.path.join(frames, f)
            if first <= i <= last:
                os.rename(path, os.path.join(frames, f"{i:05d}.png"))
            else:
                os.unlink(path)
    clip = os.path.join(out, name + ".mp4")
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-threads", "1",
                    "-framerate", "60", "-start_number",
                    str(first), "-i", os.path.join(frames, "%05d.png"), "-c:v", "libx264",
                    "-threads", str(cpu_count()), "-crf", "12", "-pix_fmt", "yuv420p", clip],
                   check=True)
    sheet(frames, first, last, os.path.join(out, name + "-sheet.jpg"))
    return clip


def sheet(frames, first, last, path):
    """Six frames, evenly spread, in a 3 x 2 grid at half size, each labelled by its frame."""
    picks = [first + (last - first) * k // 5 for k in range(6)]
    inputs = []
    for p in picks:
        inputs += ["-threads", "1", "-i", os.path.join(frames, f"{p:05d}.png")]
    labelled = ";".join(
        f"[{k}:v]scale=iw/2:ih/2,drawtext=text='{p}':x=8:y=8:fontsize=24:fontcolor=white:"
        f"box=1:boxcolor=black@0.5[v{k}]" for k, p in enumerate(picks))
    grid = "".join(f"[v{k}]" for k in range(6))
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", *inputs,
                    "-filter_complex_threads", str(cpu_count()), "-filter_complex",
                    f"{labelled};{grid}xstack=inputs=6:layout=0_0|w0_0|w0+w1_0|0_h0|w0_h0|w0+w1_h0",
                    "-threads", str(cpu_count()), "-q:v", "3", path], check=True)


def main():
    args = sys.argv[1:]
    if not args:
        sys.exit(__doc__)
    size = "1280x720"
    if "--size" in args:
        i = args.index("--size")
        size = args[i + 1]
        del args[i:i + 2]
    out, names = args[0], args[1:] or list(CAPTURES)
    unknown = [n for n in names if n not in CAPTURES]
    if unknown:
        sys.exit(f"no capture {unknown}: the captures are {', '.join(CAPTURES)}")
    os.makedirs(out, exist_ok=True)
    for n in names:
        print(n, capture(n, out, size), flush=True)


if __name__ == "__main__":
    main()
