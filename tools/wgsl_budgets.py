#!/usr/bin/env python3
"""The WGSL size budgets (tools/check.sh): every example's, sketch's and lens's pipelines,
each built (in parallel) by the wrela CLI given, `tools/wgsl_budgets.py <wrela>`. A package whose
build bakes a history (`BAKED`: minutes when the constants' cache is cold, after a change to the
code the bake runs) is left to the long checks, `tools/wgsl_budgets.py <wrela> --baked`, which
build only those."""

import concurrent.futures, gzip, pathlib, shutil, subprocess, sys, tempfile
from subjects import SUBJECTS, lens
wrela = sys.argv[1]
baked_only = "--baked" in sys.argv[2:]
BAKED = {pathlib.Path("examples/last-green")}
out = tempfile.mkdtemp()
try:
    hello = "examples/hello-field"
    # The lens on each subject (tools/subjects.py).
    if not baked_only:
        list(concurrent.futures.ThreadPoolExecutor().map(
            lambda s: subprocess.run([wrela, "studio", f"examples/{s}", "build"], check=True, capture_output=True), SUBJECTS))
    # Each package, where it's built, and how. A lens is built lifted, as `wrela studio` built it
    # just now (its page, beside the subject): its WGSL is read from there.
    packages = [(p, pathlib.Path(out) / p.name, True)
                for p in sorted(m.parent for root in ["examples", "compiler/tests/sketches", "ui/tests"]
                                for m in pathlib.Path(root).glob("*/main.wrela"))]
    if not baked_only:
        packages += [(pathlib.Path(lens(s)), pathlib.Path(f"examples/{s}/build/studio/page"), False)
                     for s in SUBJECTS]
    packages = [p for p in packages if (p[0] in BAKED) == baked_only]
    def build(package):
        pkg, built, needed = package
        if needed:
            subprocess.run([wrela, "build", str(pkg), "-o", str(built)], check=True, capture_output=True)
        return pkg, built
    # Each pipeline's WGSL: what the browser downloads and the driver compiles (pipeline
    # creation time is budgeted by the long checks). Hello field keeps its own budget; every
    # other example's and sketch's pipelines are at most 256 KiB each.
    pipelines = 0
    for pkg, built in concurrent.futures.ThreadPoolExecutor().map(build, packages):
        raw_budget, gz_budget = (128 * 1024, 24 * 1024) if str(pkg) == hello else (256 * 1024, None)
        largest = 0
        for wgsl in sorted(built.glob("*.wgsl")):
            pipelines += 1
            text = wgsl.read_bytes()
            raw, packed = len(text), len(gzip.compress(text))
            if raw > raw_budget or (gz_budget and packed > gz_budget):
                sys.exit(f"{pkg}/{wgsl.name} is {raw / 1024:.0f} KiB ({packed / 1024:.0f} gzipped), over its size budget")
            largest = max(largest, raw)
        print(f"  {pkg}: largest pipeline {largest / 1024:.0f} KiB (budget {raw_budget // 1024})")
    print(f"  {pipelines} pipelines in {len(packages)} packages")
finally:
    shutil.rmtree(out, ignore_errors=True)
