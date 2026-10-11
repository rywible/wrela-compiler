#!/usr/bin/env python3
"""The WGSL size budgets (tools/check.sh): every example's, sketch's and lens's pipelines,
each built (in parallel) by the wrela CLI given, `tools/wgsl_budgets.py <wrela>`. A package whose
build bakes a history (`BAKED`: minutes when the constants' cache is cold, after a change to the
code the bake runs) is left to the long checks, `tools/wgsl_budgets.py <wrela> --baked`, which
build only those. With `--memo`, each build's sizes are remembered in the gate's memory
(tools/memo.py) by the CLI and the files the build read: a build whose CLI and files are as they
were doesn't run again."""

import concurrent.futures, gzip, os, pathlib, shutil, subprocess, sys
from resources import cpu_count
from subjects import SUBJECTS, lens
wrela = sys.argv[1]
baked_only = "--baked" in sys.argv[2:]
memo = None
if "--memo" in sys.argv[2:]:
    import memo as gate
    memo = gate.Memo(os.getcwd(), os.environ.get("CARGO_TARGET_DIR", "target"), gate.stores()[0],
                     cli=wrela)
BAKED = {pathlib.Path("examples/last-green")}
# Under target/, at the same paths each time: what a build reads there isn't its input.
out = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", "target")) / "wgsl-budgets"


def sizes(built, compressed_budget=False):
    """WGSL bytes and, only when budgeted or over budget, bytes gzipped.

    Most pipelines have only a raw size budget. Compressing every successful shader spends
    CPU time without checking anything; failures still include both sizes in their report.
    """
    return {w.name: [len(t), len(gzip.compress(t))
                     if compressed_budget or len(t) > 256 * 1024 else None]
            for w in sorted(built.glob("*.wgsl")) for t in [w.read_bytes()]}


def build(package):
    """A package's pipelines' sizes, from the build `command` makes in `built`."""
    pkg, command, built = package
    compressed_budget = str(pkg) == "examples/hello-field"

    def run():
        if out in built.parents:
            shutil.rmtree(built, ignore_errors=True)
        return command

    if memo is None:
        subprocess.run(run(), check=True, capture_output=True)
        return pkg, sizes(built, compressed_budget)
    key = memo.rebuilds.key(os.path.abspath(pkg), ["wgsl sizes", command[1]])
    got = memo.traced(key, run, os.getcwd(), lambda: sizes(built, compressed_budget), output=built)
    if got is None:
        sys.exit(f"{pkg}: `{' '.join(command)}` failed")
    return pkg, got


try:
    hello = "examples/hello-field"
    # The lens on each subject (tools/subjects.py), first: inspect its shader artifacts without
    # compiling the WASM for the native host, since this check never runs it. A lens is built
    # lifted, as `wrela studio` builds it (its page, beside the subject): its WGSL is read there.
    lenses = [] if baked_only else [
        (pathlib.Path(lens(s)), [wrela, "studio", f"examples/{s}", "build", "--artifacts-only"],
         pathlib.Path(f"examples/{s}/build/studio/page")) for s in SUBJECTS]
    packages = [(p, [wrela, "build", str(p), "-o", str(out / p.name)], out / p.name)
                for p in sorted(m.parent for root in ["examples", "compiler/tests/sketches", "ui/tests"]
                                for m in pathlib.Path(root).glob("*/main.wrela"))
                if (p in BAKED) == baked_only]
    with concurrent.futures.ThreadPoolExecutor(max_workers=cpu_count()) as pool:
        built = list(pool.map(build, lenses))
        built += list(pool.map(build, packages))
    # Each pipeline's WGSL: what the browser downloads and the driver compiles (pipeline
    # creation time is budgeted by the long checks). Hello field keeps its own budget; every
    # other example's and sketch's pipelines are at most 256 KiB each.
    pipelines = 0
    for pkg, pipes in built:
        raw_budget, gz_budget = (128 * 1024, 24 * 1024) if str(pkg) == hello else (256 * 1024, None)
        largest = 0
        for name, (raw, packed) in sorted(pipes.items()):
            pipelines += 1
            if raw > raw_budget or (gz_budget and packed > gz_budget):
                sys.exit(f"{pkg}/{name} is {raw / 1024:.0f} KiB ({packed / 1024:.0f} gzipped), over its size budget")
            largest = max(largest, raw)
        print(f"  {pkg}: largest pipeline {largest / 1024:.0f} KiB (budget {raw_budget // 1024})")
    print(f"  {pipelines} pipelines in {len(built)} packages")
finally:
    shutil.rmtree(out, ignore_errors=True)
