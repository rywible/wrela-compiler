#!/usr/bin/env bash
# Makes a Claude Code cloud machine (Ubuntu 24.04, 4 cores, no GPU) a wrela dev machine: the
# GPU is the CPU, through Mesa's lavapipe (Vulkan on llvmpipe), patched and built here.
#
#   tools/cloud.sh          provisions the machine, once (as root): apt packages, the Rust
#                           toolchain in rust-toolchain.toml, lavapipe patched and installed as
#                           the system's Vulkan driver, the browser runtime's packages. A second
#                           run checks the marker and exits; while another run provisions, it
#                           waits for it. About 3 minutes; the cloud environment's setup script
#                           runs it, so the environment's cache keeps it (else --warm does, at
#                           a session's start).
#   tools/cloud.sh --check  exits 0 if this version of the script provisioned the machine.
#   tools/cloud.sh --warm   provisions the machine if it isn't, then builds what a session
#                           builds first, slowly (nice): the CLI and the native host, the floor
#                           and its bake, the tests. tools/hooks.py starts it in the background
#                           at a session's start; its log is /tmp/wrela-warm.log.
#   tools/cloud.sh --env    prints the environment a session needs (KEY=VALUE lines): that the
#                           GPU is software (tools/check.sh reports its time budget, not gates
#                           it), and Chromium for tools/headless.py, on SwiftShader's WebGPU
#                           (Chrome doesn't offer lavapipe as a WebGPU adapter).
#
# Why patched: in llvmpipe, each flush of the draw module's vertex buffer reset the id of every
# vertex of the whole draw, not of the vertices it emitted (tools/cloud/mesa-vbuf-reset.patch).
# A draw that is clipped (the floor's vegetation, near the eye) paid the draw's size at each
# flush: 17% of the floor's CPU time, on one thread. Patched, 41 frames of the floor at 1080p
# take 44 s, not 70 s, with the same pixels. SwiftShader (Chromium's) ran out of 16 GB before
# frame 11 of the floor; thread counts, vector width and -march made no difference.
set -euo pipefail
cd "$(dirname "$0")/.."

PREFIX=/opt/wrela-mesa
PATCH=tools/cloud/mesa-vbuf-reset.patch
# The marker names this script's and the patch's version: a change to either provisions again.
STAMP="$PREFIX/provisioned-$(cat "$0" "$PATCH" | sha256sum | cut -c1-16)"

case "${1:-}" in
  --check) [ -e "$STAMP" ]; exit ;;
  --env)
    echo "WRELA_SOFTWARE_GPU=1"
    chrome=$(ls -d /opt/pw-browsers/chromium-*/chrome-linux/chrome 2>/dev/null | sort -V | tail -1 || true)
    if [ -n "$chrome" ]; then
      echo "WRELA_CHROME=$chrome"
      echo "WRELA_CHROME_ARGS=--no-sandbox --enable-unsafe-webgpu --enable-features=Vulkan --use-angle=swiftshader --use-webgpu-adapter=swiftshader --ignore-gpu-blocklist"
    fi
    exit 0 ;;
  --warm)
    # Each step is what a session's first build or render would do: a step that fails leaves
    # the session to find out, as it would have.
    exec > /tmp/wrela-warm.log 2>&1
    export CARGO_TERM_COLOR=never
    s=$SECONDS
    "$0" || exit 1
    nice -n 10 cargo build -q --release -p wrela-host -p wrela && echo "built the CLI and the host: $((SECONDS - s))s"
    nice -n 10 target/release/wrela build examples/last-green > /dev/null && echo "built the floor: $((SECONDS - s))s"
    nice -n 10 cargo test -q --release --workspace --no-run 2> /dev/null && echo "built the tests: $((SECONDS - s))s"
    echo "warm: $((SECONDS - s))s"
    exit 0 ;;
  "") ;;
  *) echo "usage: tools/cloud.sh [--check | --warm | --env]" >&2; exit 2 ;;
esac

# One provisioning at a time: a second run (a session's command while the hook's runs) waits
# for the first, then finds the marker.
exec 9> /tmp/wrela-provision.lock
flock 9
[ -e "$STAMP" ] && exit 0
if [ "$(id -u)" != 0 ]; then echo "tools/cloud.sh provisions as root" >&2; exit 1; fi
s=$SECONDS
export DEBIAN_FRONTEND=noninteractive

# Mesa's source, the version Ubuntu ships, from Ubuntu's archive (the cloud's network allows
# it; Mesa's own servers it doesn't).
sed -i 's/^Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/ubuntu.sources
apt-get update -q
apt-get install -y -q mesa-vulkan-drivers vulkan-tools linux-tools-generic
version=$(dpkg-query -W -f '${Version}' mesa-vulkan-drivers)
apt-get build-dep -y -q "mesa=$version"
echo "apt: $((SECONDS - s))s"

# The Rust toolchain rust-toolchain.toml names, with its targets; the crates, fetched.
rustup show active-toolchain > /dev/null 2>&1 || rustup toolchain install
cargo fetch -q
echo "rust: $((SECONDS - s))s"

# The browser runtime's packages (TypeScript, the WebGPU types), for the gate's browser checks.
(cd runtime/browser && bun install --frozen-lockfile)

# lavapipe, patched: only the Vulkan driver, with the stock one's window systems (Chromium's GPU
# process starts Vulkan with their extensions, and fails without them).
src=$(mktemp -d)
(cd "$src" && apt-get source -q "mesa=$version" > /dev/null)
mesa=$(ls -d "$src"/mesa-*/ | head -1)
patch -d "$mesa" -p1 < "$PATCH"
meson setup "$src/build" "$mesa" --prefix="$PREFIX" -Dbuildtype=release -Db_ndebug=true \
  -Dvulkan-drivers=swrast -Dgallium-drivers=llvmpipe -Dplatforms=x11,wayland -Dglx=disabled -Degl=disabled \
  -Dgles1=disabled -Dgles2=disabled -Dopengl=false -Dllvm=enabled -Dshared-llvm=enabled \
  -Dvideo-codecs= -Dgallium-va=disabled -Dgallium-vdpau=disabled -Dvulkan-layers= -Dtools= \
  -Dvalgrind=disabled -Dlibunwind=disabled -Dlmsensors=disabled -Dzstd=enabled -Dexpat=disabled \
  -Dgbm=disabled > "$src/meson.log"
ninja -C "$src/build" install > "$src/ninja.log"
lib=$(find "$PREFIX" -name libvulkan_lvp.so | head -1)
# The system's lavapipe is this one: the loader finds it with no variable set, and a Vulkan
# program sees one CPU adapter.
icd=/usr/share/vulkan/icd.d/lvp_icd.json
python3 - "$icd" "$lib" <<'PY'
import json, sys
icd, lib = sys.argv[1:]
d = json.load(open(icd))
d["ICD"]["library_path"] = lib
json.dump(d, open(icd, "w"), indent=4)
PY
rm -rf "$src"
echo "lavapipe: $((SECONDS - s))s"

rm -f "$PREFIX"/provisioned-*
touch "$STAMP"
vulkaninfo --summary 2>/dev/null | grep -m1 deviceName || true
echo "provisioned in $((SECONDS - s))s"
