# Agent syntax test, version 2: results

## m2 (2026-10-24): main (0.4.1) against this branch (0.5.0)

Claude Sonnet 5.5, the full context set, the same 30 tasks and brief, each attempt graded by the
hidden tests (validator: the branch's compiler, for both). `m2-main.json` and `m2-branch.json`
are score.py's reports; `m2-*/<task>/` hold each attempt's sources, diagnostics, status and
grade, and the agent's report.

| | main (0.4.1) | branch (0.5.0) |
|---|---|---|
| first try correct | 30 of 30 | 30 of 30 |
| mean attempts to correct | 1.0 | 1.0 |
| first try builds | 30 of 30 | 30 of 30 |
| mean tokens per task | 607,450 | 608,121 |
| mean cost per task | $0.71 | $0.71 |

Both compilers are at the ceiling: no agent met a diagnostic, so these tasks can't show what the
branch's diagnostics, lints and docs change. The next version needs harder tasks (several
modules, ownership across calls, GPU passes whose output is read back and graded), a weaker or
cheaper model as well, and fewer context files, so the score moves.

Gaps the agents named in their reports (each "misled me" item, by task), which the spec or std's
docs should close:

- `std::math::PI`'s type isn't given (01, 03, 26).
- Which way `FragCoord`'s y runs isn't stated (08).
- `Arena`'s API (`new`, `insert`, iterating it) isn't in the spec; only `wrela doc` had it (22).
- `write`'s values argument type, and whether `bind` mixes positional and named arguments (20, 29).
- `certify_clear` and `round_cone` have no signatures in the spec's prose (06, 27).
- `sphere(0.5)` and `sphere(radius: 0.2)` both appear; which is the form isn't said (19).
- The agents could compile but not run their programs (only `attempt` is allowed), so none
  checked a value before stopping: a v3 brief could allow `wrela test`.

Two attempts were rerun from nothing (`run.py --resume`) after a full disk cut their logs short:
`m2-main/28-frame-script` and `m2-branch/29-gpu-uniform`.
