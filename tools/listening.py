#!/usr/bin/env python3
"""Blind listening tests of wrela's piano (#48, #56): renders matched in loudness, shuffled, and
the key kept apart until the listener has answered.

    tools/listening.py round [out-dir]
    tools/listening.py gate [out-dir]
    tools/listening.py score <out-dir> <answers>

Spike 14's round: four renders of the first Gymnopédie, for a person to rank blind. They are
{wrela's piano, a sampled piano} x {the deadpan, the performance}. wrela's are the piece's dry
takes (examples/gymnopedie takes 2 and 3: no room, as the sampled piano has none); the sampled
piano plays each take's MIDI file (`wrela audio ... midi`): the Salamander Grand Piano V3
(Alexander Holm, CC BY 3.0), the whole instrument (FreePats' retuned SFZ, with its velocity
tracking, release samples and pedal noise), played by sfizz. They're written as A.wav to D.wav;
which is which goes in key-open-after-ranking.json. With them, outside the test: the
performance as the game plays it, with the room (wrela-performance-with-room.wav).

The piano gate (#56 AC4): 16 blind pairs, each one excerpt played by both pianos from the same
performance: the first Gymnopédie's (take 3, dry) and the test chords' (examples/chords, take 2,
dry). Each pair is `pair-NN/A.wav` and `pair-NN/B.wav`, in an order of its own; the listener writes
the letter they prefer for each pair in `answers.txt` (one line a pair: `01 A`), and `score`
counts the pairs where it's wrela's piano, against the key (key-open-after-answering.json). The
gate passes at 12 of 16 (p < 0.05 against a coin toss).

Every render is scaled to the same RMS level (the loudest the set allows with no peak past
-1 dBFS), and padded with silence to the longest, so neither loudness nor length gives one away.

Needs `wrela` built (`cargo build --release -p wrela`), sfizz's `sfizz_render` (under
$WRELA_SFIZZ, or ~/.cache/wrela/sfizz: its `bin/` and `lib/`), and the SFZ (at
$WRELA_REFERENCE_SFZ, or under ~/.cache/wrela/salamander-sfz/). Run from the repository's root.
"""

import json
import math
import os
import secrets
import struct
import subprocess
import sys
from pathlib import Path

PIECE = "examples/gymnopedie"
WRELA = "target/release/wrela"
SFZ_NAME = "SalamanderGrandPianoRetuned-V3+20200602.sfz"
CACHE = Path.home() / ".cache/wrela"


def reference_sfz():
    """The reference piano's SFZ file: $WRELA_REFERENCE_SFZ, or the first under the cache."""
    if "WRELA_REFERENCE_SFZ" in os.environ:
        return Path(os.environ["WRELA_REFERENCE_SFZ"])
    found = sorted((CACHE / "salamander-sfz").glob(f"**/{SFZ_NAME}"))
    return found[0] if found else CACHE / "salamander-sfz" / SFZ_NAME


def sfizz():
    """sfizz_render, and the environment that finds its libraries."""
    root = Path(os.environ.get("WRELA_SFIZZ", str(CACHE / "sfizz")))
    lib = str(root / "lib")
    env = {**os.environ, "DYLD_LIBRARY_PATH": lib, "LD_LIBRARY_PATH": lib}
    return root / "bin" / "sfizz_render", env


def run(*args, env=None):
    r = subprocess.run(args, capture_output=True, text=True, env=env)
    if r.returncode != 0:
        sys.exit(f"{' '.join(map(str, args))} failed:\n{r.stdout}{r.stderr}")
    return r.stdout


def read_wav(path):
    """A WAV file's samples as floats (channels interleaved), its channels and its rate. Reads
    32-bit float, and 16- and 24-bit PCM."""
    data = Path(path).read_bytes()
    at, fmt, channels, rate, bits = 12, None, 0, 0, 0
    while at + 8 <= len(data):
        chunk, size = data[at : at + 4], struct.unpack("<I", data[at + 4 : at + 8])[0]
        body = data[at + 8 : at + 8 + size]
        if chunk == b"fmt ":
            fmt, channels, rate = struct.unpack("<HHI", body[:8])
            bits = struct.unpack("<H", body[14:16])[0]
            if fmt == 0xFFFE:
                fmt = struct.unpack("<H", body[24:26])[0]
        elif chunk == b"data":
            if (fmt, bits) == (3, 32):
                return list(struct.unpack(f"<{len(body) // 4}f", body)), channels, rate
            if (fmt, bits) == (1, 16):
                return [x / 32768 for x in struct.unpack(f"<{len(body) // 2}h", body)], channels, rate
            if (fmt, bits) == (1, 24):
                samples = [int.from_bytes(body[i : i + 3], "little", signed=True) / 8388608 for i in range(0, len(body) - 2, 3)]
                return samples, channels, rate
            sys.exit(f"{path}: WAV format {fmt} at {bits} bits isn't read")
        at += 8 + size + (size % 2)
    sys.exit(f"{path} has no data")


def write_wav(path, samples, channels, rate):
    """16-bit PCM, as spike 14's round 3 was written: x * 32767, rounded."""
    ints = [max(-32768, min(32767, round(x * 32767))) for x in samples]
    data = struct.pack(f"<{len(ints)}h", *ints)
    fmt = struct.pack("<HHIIHH", 1, channels, rate, rate * 2 * channels, 2 * channels, 16)
    header = b"RIFF" + struct.pack("<I", 4 + 8 + len(fmt) + 8 + len(data)) + b"WAVE"
    Path(path).write_bytes(header + b"fmt " + struct.pack("<I", len(fmt)) + fmt + b"data" + struct.pack("<I", len(data)) + data)


def rms(samples):
    return math.sqrt(sum(x * x for x in samples) / max(len(samples), 1))


def matched(loaded):
    """The gain for each render in `loaded` (a name to samples) that brings all to one RMS level:
    as loud as it can be with every peak at -1 dBFS or below."""
    levels = {k: rms(s) for k, s in loaded.items()}
    peaks = {k: max(abs(x) for x in s) for k, s in loaded.items()}
    ceiling = 10 ** (-1 / 20)
    target = min(levels[k] * ceiling / peaks[k] for k in loaded)
    return {k: target / levels[k] for k in loaded}


def blind(loaded, out, letters, key_name, rng=None):
    """Writes the renders in `loaded` (a name to (samples, channels, rate) each) shuffled, as
    `<letter>.wav` in `out`, matched in loudness and length; and the key, which says which is
    which, as `key_name`. The key, as a dict."""
    longest = max(len(s) for s, _, _ in loaded.values())
    loaded = {k: (s + [0.0] * (longest - len(s)), c, r) for k, (s, c, r) in loaded.items()}
    gains = matched({k: s for k, (s, _, _) in loaded.items()})
    order = list(loaded)
    (rng or secrets.SystemRandom()).shuffle(order)
    key = {}
    for letter, k in zip(letters, order):
        samples, channels, rate = loaded[k]
        gain = gains[k]
        write_wav(Path(out) / f"{letter}.wav", [x * gain for x in samples], channels, rate)
        key[letter] = {"render": k, "gain_db": round(20 * math.log10(gain), 2), "seconds": round(len(samples) / channels / rate, 1)}
    (Path(out) / key_name).write_text(json.dumps(key, indent=2) + "\n")
    return key


def round_of_four(out):
    work = out / "work"
    work.mkdir(parents=True, exist_ok=True)
    sfz = reference_sfz()
    if not sfz.is_file():
        sys.exit(f"no SFZ at {sfz}: set WRELA_REFERENCE_SFZ")
    render, env = sfizz()
    if not render.is_file():
        sys.exit(f"no {render}: set WRELA_SFIZZ")
    renders = {}
    for take, name in ((2, "deadpan"), (3, "performance")):
        wav = work / f"wrela-{name}.wav"
        run(WRELA, "audio", PIECE, "wav", str(wav), "--take", str(take))
        renders[f"wrela piano, {name}"] = wav
        mid = work / f"{name}.mid"
        run(WRELA, "audio", PIECE, "midi", str(mid), "--take", str(take))
        ref = work / f"sampled-{name}.wav"
        run(str(render), "--sfz", str(sfz), "--midi", str(mid), "--wav", str(ref), "--samplerate", "48000", "--quality", "3", env=env)
        renders[f"sampled piano, {name}"] = ref
    run(WRELA, "audio", PIECE, "wav", str(out / "wrela-performance-with-room.wav"), "--take", "1")
    loaded = {k: read_wav(v) for k, v in renders.items()}
    key = blind(loaded, out, "ABCD", "key-open-after-ranking.json")
    print(json.dumps({"files": [f"{out}/{letter}.wav" for letter in key], "key": str(out / "key-open-after-ranking.json")}, indent=2))


# The gate's excerpts: (piece, its dry take, first bar, last bar). Ten of the Gymnopédie (bars as
# played: the repeat written out, so 71 to 78 is the second ending), six of the test chords
# (soft, loud, low, high, high and loud, pedalled).
GATE = [
    *[(PIECE, 3, a, b) for a, b in ((5, 8), (9, 12), (13, 16), (21, 24), (25, 28), (29, 31), (32, 35), (36, 39), (71, 74), (75, 78))],
    *[("examples/chords", 2, a, b) for a, b in ((1, 4), (9, 12), (17, 20), (25, 28), (29, 32), (33, 38))],
]

# How long an excerpt rings past its last bar (s), and its fades in and out (s).
RING = 1.5
FADE_IN = 0.02
FADE_OUT = 0.3
GATE_PASS = 12


def cut(samples, channels, rate, start, end):
    """`samples` (channels interleaved) from `start` to `end` seconds, faded in and out."""
    a, b = int(start * rate) * channels, min(int(end * rate) * channels, len(samples))
    out = samples[a:b]
    frames = len(out) // channels
    for i in range(frames):
        t = i / rate
        g = min(1.0, t / FADE_IN, (frames - 1 - i) / rate / FADE_OUT)
        for c in range(channels):
            out[i * channels + c] *= g
    return out


def excerpt_times(described, first, last):
    """When bar `first` starts and bar `last` ends (s), from a take's `describe`."""
    beats = described["beats"]
    # The beats run to the last bar's end.
    per = (len(beats) - 1) // described["bars"]
    return beats[(first - 1) * per], beats[min(last * per, len(beats) - 1)]


def gate(out, rng=None):
    work = out / "work"
    work.mkdir(parents=True, exist_ok=True)
    sfz = reference_sfz()
    if not sfz.is_file():
        sys.exit(f"no SFZ at {sfz}: set WRELA_REFERENCE_SFZ")
    render, env = sfizz()
    if not render.is_file():
        sys.exit(f"no {render}: set WRELA_SFIZZ")
    takes = {}
    for piece, take in sorted({(p, t) for p, t, _, _ in GATE}):
        name = Path(piece).name
        wav, mid, ref = work / f"wrela-{name}.wav", work / f"{name}.mid", work / f"sampled-{name}.wav"
        run(WRELA, "audio", piece, "wav", str(wav), "--take", str(take))
        run(WRELA, "audio", piece, "midi", str(mid), "--take", str(take))
        run(str(render), "--sfz", str(sfz), "--midi", str(mid), "--wav", str(ref), "--samplerate", "48000", "--quality", "3", env=env)
        described = json.loads(run(WRELA, "audio", piece, "describe", "--take", str(take)))
        takes[(piece, take)] = (read_wav(wav), read_wav(ref), described)
    rng = rng or secrets.SystemRandom()
    key = {}
    for n, (piece, take, first, last) in enumerate(GATE, 1):
        ours, theirs, described = takes[(piece, take)]
        start, end = excerpt_times(described, first, last)
        loaded = {
            "wrela": cut(list(ours[0]), ours[1], ours[2], start, end + RING),
            "reference": cut(list(theirs[0]), theirs[1], theirs[2], start, end + RING),
        }
        gains = matched(loaded)
        order = ["wrela", "reference"]
        rng.shuffle(order)
        pair = out / f"pair-{n:02}"
        pair.mkdir(exist_ok=True)
        for letter, k in zip("AB", order):
            write_wav(pair / f"{letter}.wav", [x * gains[k] for x in loaded[k]], ours[1], ours[2])
        key[f"{n:02}"] = {"wrela": "AB"[order.index("wrela")], "piece": Path(piece).name, "bars": [first, last]}
    (out / "key-open-after-answering.json").write_text(json.dumps(key, indent=2) + "\n")
    (out / "answers.txt").write_text("".join(f"{n:02} \n" for n in range(1, len(GATE) + 1)))
    print(json.dumps({"pairs": len(GATE), "answers": str(out / "answers.txt"), "key": str(out / "key-open-after-answering.json")}, indent=2))


def binomial_p(k, n):
    """The chance of `k` or more heads in `n` tosses of a fair coin."""
    return sum(math.comb(n, i) for i in range(k, n + 1)) / 2**n


def score(key, answers):
    """How many of the answered pairs chose wrela's piano, of how many, the chance a coin does as
    well, and whether the gate passes. `key` is the gate's key; `answers` the answers' text."""
    picked = {}
    for line in answers.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].upper() in ("A", "B"):
            picked[parts[0].zfill(2)] = parts[1].upper()
    missing = sorted(set(key) - set(picked))
    if missing:
        sys.exit(f"no answer for pairs {', '.join(missing)}")
    wrela = sum(picked[n] == key[n]["wrela"] for n in key)
    return {"wrela": wrela, "of": len(key), "p": round(binomial_p(wrela, len(key)), 4), "passes": wrela >= GATE_PASS}


def main(argv):
    if len(argv) >= 2 and argv[1] == "round":
        round_of_four(Path(argv[2] if len(argv) > 2 else f"{PIECE}/build/listening"))
    elif len(argv) >= 2 and argv[1] == "gate":
        gate(Path(argv[2] if len(argv) > 2 else f"{PIECE}/build/gate"))
    elif len(argv) == 4 and argv[1] == "score":
        key = json.loads((Path(argv[2]) / "key-open-after-answering.json").read_text())
        print(json.dumps(score(key, Path(argv[3]).read_text()), indent=2))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
