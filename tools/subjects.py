"""The lens's subjects (AC12 of #39; the great tree and the fawn, #51), which tools/check.sh and
tools/wgsl_budgets.py build: each one's directory in examples/. `wrela studio` writes the lens's
program beside a subject (build/studio/lens), which builds lifted."""

SUBJECTS = ["wolf", "grazer", "great-tree", "fawn"]


def package(subject):
    """The name of a subject's package: its directory's, with `_` for `-`."""
    return subject.replace("-", "_")


def lens(subject):
    """Where `wrela studio` writes the lens's program on a subject."""
    return f"examples/{subject}/build/studio/lens"
