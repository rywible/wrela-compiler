#!/usr/bin/env python3
"""A play round of the floor (#55 AC5): examples/last-green played in headless Chrome stable at
1080p, paced at 60 Hz as in play, eight threads, through the play harness's keys, then its
lookbook rendered again. Everything goes to target/rounds/round-<n>/.

    tools/round.py <n> [--quick [--every <k>] | --minutes 30|60] [--no-lookbook] [--frames <n>]

`--quick` (about two minutes, the edit loop's round): a travel to every fourth site (each
region's), at each a run of 2 s and a look round, then six lookbook shots held, the screen saved
at each (`snaps/`); no lookbook render after. `--every 1` travels to every site (every region:
about five minutes), as the last round does.

The floor must be built (`wrela build examples/last-green`). The round plays, as a player would
through the harness's taps: the floor's walk (P: from the gate, along the critical path, through
every region and the water, past each lookbook place: 25 minutes at a run); then each lookbook
shot held 15 s (L, its number, Enter; Escape); with `--minutes 60`, then the egg's road in the
wildwood (travel, Y) for 10 minutes, and the floor's walk again. It saves:

- `ticks.log`, the round's replay (each tick's records and state hash: `wrela-host --replay`);
- `log.txt`, what the floor printed (the harness's report every 2 s, late tiles, the playable
  frame), and `console.log`;
- `summary.json` and what it prints: the frames' intervals (a frame later than 33 ms after the
  last is a hitch), each hitch with where the player was, the tiles late, the regions crossed
  and the sites passed, the deepest water stood in;
- `lookbook/`, each shot's still (tools/lookbook.py --stills), to set beside the last round's.

`--frames` cuts the round short (to try the tool).
"""

import json, os, shutil, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BUILD = os.path.join(ROOT, "examples/last-green/build")
HEADLESS = os.path.join(ROOT, "tools/headless.py")
LOOKBOOK = os.path.join(ROOT, "tools/lookbook.py")
FPS = 60
WALK_FRAMES = 25 * 60 * FPS + 30 * FPS  # the floor's walk at a run (25.04 min), and slack
SHOT_FRAMES = 15 * FPS
ROAD_FRAMES = 10 * 60 * FPS


def shot_names():
    import re
    text = open(os.path.join(ROOT, "examples/last-green/lookbook.wrela")).read()
    return re.findall(r'Shot \{\s*name: "([^"]+)"', text)


def sites():
    """How many sites the floor's map has (the travels past them: the egg's place, its road)."""
    return open(os.path.join(ROOT, "examples/last-green/map.wrela")).read().count("sites.push")


def keys_at(frame, keys):
    return [{"frame": frame, "type": "key", "key": k} for k in keys]


def script(minutes):
    """The round's input events, and how many frames it runs."""
    events = keys_at(1, ["KeyP"])
    f = 1 + WALK_FRAMES
    for i, _ in enumerate(shot_names()):
        events += keys_at(f, ["KeyL", *["Digit" + d for d in str(i)], "Enter"])
        f += SHOT_FRAMES
    events += keys_at(f, ["Escape"])
    f += FPS
    if minutes >= 60:
        road = sites() + 1
        events += keys_at(f, ["KeyG", *["Digit" + d for d in str(road)], "Enter"])
        f += 5 * FPS
        events += keys_at(f, ["KeyY"])
        f += ROAD_FRAMES
        events += keys_at(f, ["KeyY"])
        f += FPS
        events += keys_at(f, ["KeyG", "Digit0", "Enter"])
        f += 5 * FPS
        events += keys_at(f, ["KeyP"])
        f += WALK_FRAMES
    return events, max(f, minutes * 60 * FPS)


QUICK_SHOTS = [1, 2, 3, 5, 7, 12]


SNAP = 130


def quick_script(every=4):
    """The quick round's events, how many frames, and the frames whose screens are saved
    (every `SNAP` frames: each site's block is two, each shot's three)."""
    events = []
    f = SNAP
    for site in range(0, sites(), every):
        events += keys_at(f + 5, ["KeyG", *["Digit" + d for d in str(site)], "Enter"])
        events += keys_at(f + SNAP + 5, ["KeyT", "KeyR"])
        events += keys_at(f + 2 * SNAP - 25, ["KeyT", "KeyR", "KeyE", "KeyE", "KeyE"])
        f += 2 * SNAP
    for shot in QUICK_SHOTS:
        events += keys_at(f + 5, ["KeyL", *["Digit" + d for d in str(shot)], "Enter"])
        f += 3 * SNAP
    events += keys_at(f + 5, ["Escape"])
    frames = f + 10
    return events, frames, list(range(SNAP, frames, SNAP))


def summarize(out, frames_json, log_lines, printed_in, snaps=()):
    began = frames_json["began_ms"]
    gaps = [(i, began[i] - began[i - 1]) for i in range(1, len(began))]
    start = next((printed_in[k] for k, l in enumerate(log_lines) if l.startswith("playable")), 0)
    played = [g for g in gaps if g[0] > start]
    reports = []
    for k, line in enumerate(log_lines):
        if line.startswith('{"report"'):
            try:
                reports.append((printed_in[k], json.loads(line)))
            except json.JSONDecodeError:
                pass

    def where(frame):
        best = min(reports, key=lambda r: abs(r[0] - frame), default=None)
        if not best:
            return "?"
        r = best[1]
        return f"{r['region']}, {r['site_m']:.0f} m from {r['site']} ({r['at'][0]:.0f}, {r['at'][1]:.0f})"

    # (A frame after a saved screen waited for its readback.)
    hitches = [(i, ms) for i, ms in played if ms > 33.4 and i - 1 not in snaps]
    sorted_ms = sorted(ms for _, ms in played)
    pct = lambda q: sorted_ms[min(len(sorted_ms) - 1, int(q * len(sorted_ms)))] if sorted_ms else 0
    regions, passed = [], []
    for _, r in reports:
        if not regions or regions[-1] != r["region"]:
            regions.append(r["region"])
        if r["site_m"] < 25 and r["site"] not in passed:
            passed.append(r["site"])
    summary = {
        "frames": len(began),
        "minutes": round((began[-1] - began[0]) / 60000, 1) if began else 0,
        "playable_at": start,
        "interval_ms": {"median": pct(0.5), "p99": pct(0.99), "p999": pct(0.999),
                        "worst": sorted_ms[-1] if sorted_ms else 0},
        "hitches": [{"frame": i, "ms": round(ms, 1), "where": where(i)} for i, ms in hitches],
        "late_tiles": [l for l in log_lines if l.startswith("late tile") or "didn't come" in l],
        "regions": regions,
        "regions_seen": sorted(set(regions)),
        "sites_passed": passed,
        "deepest_water": max((r["water"] for _, r in reports), default=0),
        "reports": len(reports),
    }
    with open(os.path.join(out, "summary.json"), "w") as f:
        json.dump(summary, f, indent=1)
    print(f"{summary['frames']} frames, {summary['minutes']} min; playable at frame {start}")
    iv = summary["interval_ms"]
    print(f"frame intervals: median {iv['median']:.1f} ms, 99th {iv['p99']:.1f}, 99.9th {iv['p999']:.1f}, worst {iv['worst']:.1f}")
    print(f"{len(hitches)} frames over 33 ms after the last:")
    for h in summary["hitches"][:20]:
        print(f"  frame {h['frame']}: {h['ms']} ms, in {h['where']}")
    print(f"{len(summary['late_tiles'])} tiles late; regions: {', '.join(summary['regions_seen'])}")
    print(f"{len(passed)} sites passed within 25 m; deepest water stood in {summary['deepest_water']:.2f} m")
    return summary


def main():
    args = sys.argv[1:]
    if not args or not args[0].isdigit():
        sys.exit(__doc__)
    n = int(args[0])
    minutes = int(args[args.index("--minutes") + 1]) if "--minutes" in args else 30
    quick = "--quick" in args
    out = os.path.join(ROOT, "target/rounds", f"round-{n}")
    shutil.rmtree(out, ignore_errors=True)
    page = os.path.join(out, "page")
    shutil.copytree(BUILD, page, ignore=shutil.ignore_patterns("results", "run", "profile", "studio"))
    events, frames = script(minutes)
    snaps = []
    if quick:
        every = int(args[args.index("--every") + 1]) if "--every" in args else 4
        events, frames, snaps = quick_script(every)
    if "--frames" in args:
        frames = int(args[args.index("--frames") + 1])
    with open(os.path.join(page, "round-keys.json"), "w") as f:
        json.dump(events, f)
    timeout = frames // FPS * 2 + 600
    snap = f"&snap={SNAP}&snapfrom={SNAP}" if quick else ""
    hash_ = (f"#test&frames={frames}&width=1920&height=1080&fps={FPS}&workers=8&paced=1&nohash=1&ticklog=1&inflight=2{snap}"
             f"&input=round-keys.json")
    print(f"round {n}: {frames} frames ({frames / FPS / 60:.1f} min) in Chrome", flush=True)
    r = subprocess.run([sys.executable, HEADLESS, os.path.relpath(page, ROOT), hash_, str(timeout)],
                       capture_output=True, text=True)
    results = os.path.join(page, "results")
    for name in ["ticks.log", "log.txt", "console.log", "frames.json", "memory.json", "load.json"]:
        if os.path.exists(os.path.join(results, name)):
            shutil.copyfile(os.path.join(results, name), os.path.join(out, name))
    if snaps:
        os.makedirs(os.path.join(out, "snaps"), exist_ok=True)
        for k, f in enumerate(snaps):
            src = os.path.join(results, f"snap-{f}.png")
            if os.path.exists(src):
                shutil.copyfile(src, os.path.join(out, "snaps", f"{k:02d}-frame-{f}.png"))
    if r.returncode != 0:
        print(r.stderr[-3000:])
        print(f"the round's run failed (see {out}/console.log)")
    frames_json = json.load(open(os.path.join(out, "frames.json")))
    log_lines = open(os.path.join(out, "log.txt")).read().splitlines()
    summarize(out, frames_json, log_lines, frames_json["printed_in"], set(snaps))
    shutil.rmtree(page, ignore_errors=True)
    if "--no-lookbook" not in args and not quick:
        r = subprocess.run([sys.executable, LOOKBOOK, os.path.join(out, "lookbook"), "--stills"],
                           capture_output=True, text=True)
        print(r.stdout.strip().splitlines()[-1] if r.returncode == 0 else r.stderr[-2000:])
    sys.exit(r.returncode)


if __name__ == "__main__":
    main()
