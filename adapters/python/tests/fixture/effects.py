"""The adapter layer: this module is allowed to touch the filesystem and spawn
subprocesses. The effect leaves live here, at the boundary."""

from __future__ import annotations

import subprocess


def read_config(path_to_config: str) -> str:
    # A real filesystem effect — the leaf the analysis seeds as an `fs` root
    # (`open` is the ambient builtin, like TypeScript's `fetch`).
    with open(path_to_config, encoding="utf-8") as handle:
        return handle.read()


def run_tool(name: str) -> int:
    # A real subprocess effect — seeded as a `process` root.
    completed = subprocess.run([name, "--version"], capture_output=True)
    return completed.returncode
