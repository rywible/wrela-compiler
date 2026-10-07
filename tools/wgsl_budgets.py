#!/usr/bin/env python3
"""The WGSL size budgets (tools/check.sh): every example's, sketch's and lens's pipelines,
each built (in parallel) by the wrela CLI given, `tools/wgsl_budgets.py <wrela>`."""

import concurrent.futures, gzip, pathlib, shutil, subprocess, sys, tempfile
wrela = sys.argv[1]
out = tempfile.mkdtemp()
try:
    hello = "examples/hello-field"
    # The lens on each subject (AC12 of #39): `wrela studio` writes its program beside the
    # subject (build/studio/lens), which builds lifted.
    subjects = ["wolf", "grazer"]
    lens = lambda s: f"examples/{s}/build/studio/lens"
    list(concurrent.futures.ThreadPoolExecutor().map(
        lambda s: subprocess.run([wrela, "studio", f"examples/{s}", "build"], check=True, capture_output=True), subjects))
    # Each pipeline's WGSL: what the browser downloads and the driver compiles (pipeline
    # creation time is budgeted by the long checks). Hello field keeps its own budget; every
    # other example's and sketch's pipelines are at most 256 KiB each.
    packages = sorted(p.parent for root in ["examples", "compiler/tests/sketches", "ui/tests"]
                      for p in pathlib.Path(root).glob("*/main.wrela"))
    packages += [pathlib.Path(lens(s)) for s in subjects]
    def build(pkg):
        built = pathlib.Path(out) / pkg.parent.parent.parent.name / pkg.name if pkg.name == "lens" else pathlib.Path(out) / pkg.name
        lift = ["--lift", pkg.parent.parent.parent.name] if pkg.name == "lens" else []
        subprocess.run([wrela, "build", str(pkg), "-o", str(built), *lift], check=True, capture_output=True)
        return pkg, built
    pipelines = 0
    for pkg, built in concurrent.futures.ThreadPoolExecutor().map(build, packages):
        raw_budget, gz_budget = (128 * 1024, 24 * 1024) if str(pkg) == hello else (256 * 1024, None)
        for wgsl in sorted(built.glob("*.wgsl")):
            pipelines += 1
            text = wgsl.read_bytes()
            raw, packed = len(text), len(gzip.compress(text))
            if raw > raw_budget or (gz_budget and packed > gz_budget):
                sys.exit(f"{pkg}/{wgsl.name} is {raw / 1024:.0f} KiB ({packed / 1024:.0f} gzipped), over its size budget")
        largest = max((w.stat().st_size for w in built.glob("*.wgsl")), default=0)
        print(f"  {pkg}: largest pipeline {largest / 1024:.0f} KiB (budget {raw_budget // 1024})")
    print(f"  {pipelines} pipelines in {len(packages)} packages")
finally:
    shutil.rmtree(out, ignore_errors=True)
