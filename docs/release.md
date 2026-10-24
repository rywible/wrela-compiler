# Cutting a toolchain release

A release is a tag `vX.Y.Z` on `main`. The tag starts a workflow that builds the toolchain for
Linux x86-64 and macOS arm64 and publishes it as the tag's GitHub release. A workspace that pins
`toolchain = "X.Y.Z"` downloads that release (language.md §3.6, `compiler/cli/src/pin.rs`).

## 1. Choose the version

While the major version is 0:

- A new feature, or a change that a program or a tool can see, increases the minor version
  (`0.4.1` → `0.5.0`).
- A fix increases the patch version (`0.4.0` → `0.4.1`).

### Deprecations

A change that a program can see ships first as a migration: a warning whose one fix a tool
always makes, listed in `compiler/diag/src/codes.rs`'s `MIGRATIONS` with the release it came in
(new ones take W01xx codes). The old form keeps compiling, with the warning, for at least two
minor releases. When it is removed, the error that replaces it carries the same fix, and the
warning's code is retired (`RETIRED`). A workspace moves its pin, runs `wrela migrate` on each
member, and commits the result; `wrela migrate --dry-run` shows the diff first. The explain
suite's `every_migration_has_one_fix_a_tool_makes` holds every migration to one fix that isn't
the author's choice.

## 2. Bump the version in a pull request

1. Start from an up-to-date `main` and make a branch:

   ```bash
   git switch main && git pull --ff-only origin main && git switch -c release-X.Y.Z
   ```

2. Set `version` under `[workspace.package]` in `Cargo.toml` to `X.Y.Z`. This is the only place
   the version is written; every crate inherits it, and `wrela --version` reads it.
3. Run any Cargo build (for example `cargo check --workspace`) so that `Cargo.lock` takes the
   new version of each workspace crate. Commit `Cargo.toml` and `Cargo.lock` together.
4. Run `tools/check.sh`.
5. Run the agent syntax test with the release's compiler, and put its headline numbers (first
   try correct, mean attempts to correct, mean tokens) in the pull request:

   ```bash
   cargo build --release -p wrela && tools/agent-test/v2/run.py release-X.Y.Z --wrela "$PWD/target/release/wrela"
   ```

   It runs an agent per task and costs money (at most `budget_usd` in its `config.json` a task,
   30 tasks), so it runs once per release. `--dry-run` shows what it would run.
6. Push the branch and open a pull request against `main`, titled
   `Release X.Y.Z: <what the release brings>`. The bump can be in the same pull request as the
   change that it releases.

Ryan merges the pull request.

## 3. Tag the merge commit

```bash
git switch main && git pull --ff-only origin main
```

Make sure that `Cargo.toml` on `main` says `version = "X.Y.Z"`; `tools/package.py` stops if the
tag and Cargo's version are different. Then tag that commit and push only the tag:

```bash
git tag vX.Y.Z && git push origin vX.Y.Z
```

## 4. Watch the workflow

The tag starts `.github/workflows/toolchain-release.yml`:

- **build**, on `ubuntu-latest` and `macos-14`: `tools/package.py --version X.Y.Z --target
  <triple> --out dist`. It builds `wrela`, `wrela-host` and `wrela-serve` in release mode, with the
  source revision embedded, and adds Binaryen's `wasm-opt`, checked against a pinned SHA-256, and
  the licences.
- **publish**: writes one `SHA256SUMS` for both tarballs and creates the GitHub release `vX.Y.Z`
  with them.

```bash
gh run list --workflow toolchain-release.yml --limit 1
```

```bash
gh release view vX.Y.Z
```

The release must hold `wrela-X.Y.Z-x86_64-unknown-linux-gnu.tar.gz`,
`wrela-X.Y.Z-aarch64-apple-darwin.tar.gz` and `SHA256SUMS`.

The workflow only builds and publishes. Do not add checks to it: every check runs locally, in
`tools/check.sh`, before the merge.

To try the packaging before you tag, run it on this machine (the output is for this machine's
target only):

```bash
python3 tools/package.py --version X.Y.Z --target aarch64-apple-darwin --out target/package/dist
```

## 5. If the workflow fails

- **No release was published** (the build or publish job failed): if the cause is transient,
  run the failed jobs again with `gh run rerun <run-id> --failed`. If the code must change, fix
  it on `main` through a pull request and release the next patch version. Delete the unused tag
  so that it does not point at a release that does not exist:
  `git push origin :refs/tags/vX.Y.Z && git tag -d vX.Y.Z`.
- **A release was published**: never move or replace its tag or files. A workspace may already
  hold that release in `~/.wrela/toolchains/X.Y.Z`. Release the next patch version.

## 6. Use the release in `rywible/wrela`

Moving the pin is a change in `rywible/wrela`, through its own pull request:

1. Set `toolchain = "X.Y.Z"` under `[workspace]` in its root `wrela.toml`.
2. Run `wrela toolchain`: it says which toolchain runs and why. The first run downloads the
   release, checks it against `SHA256SUMS` and keeps it in `~/.wrela/toolchains/X.Y.Z`.
3. Run `wrela migrate` on each member, and commit what it changes with the pin.
4. Run that repository's checks with the new pin.

Before a release, `WRELA_TOOLCHAIN=~/projects/wrela-compiler` in a `rywible/wrela` checkout runs
this checkout's Cargo build in place of the pin, to try a change on both sides.
