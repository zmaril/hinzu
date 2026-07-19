"""A pure-looking functional core for the reference-edge rung.

`schedule_audit` LOOKS pure — it only appends to a list — but it hands
`write_audit` (a filesystem effect from the adapter layer) to `register` as a
*value*. `callHierarchy/outgoingCalls` is call-only, so it never saw that
higher-order use; the tree-sitter reference rung does, so `schedule_audit` now
reaches `fs` and the functional-core policy flags it. `pure_total` is genuinely
pure and must stay unflagged."""

from __future__ import annotations

from effects import write_audit

_HOOKS: list = []


def register(hook) -> None:
    _HOOKS.append(hook)


def schedule_audit() -> None:
    # `write_audit` is passed as a VALUE, never called here — a higher-order
    # reference. It reaches the filesystem when a hook runner later invokes it,
    # so the core is not effect-free after all.
    register(write_audit)


def pure_total(xs: list[int]) -> int:
    return sum(xs)
