"""The sanctioned adapter layer — filesystem effects are allowed to live here."""

from __future__ import annotations


def write_audit(line: str) -> None:
    # A real filesystem effect — the `fs` leaf `schedule_audit` reaches by
    # reference (`open` is the ambient builtin).
    with open("/tmp/hinzu-audit.log", "a", encoding="utf-8") as handle:
        handle.write(line)
