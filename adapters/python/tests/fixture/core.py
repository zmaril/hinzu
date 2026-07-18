"""The functional core: no effects are allowed here, however deep the call chain.
`parse` and `count_keys` are genuinely pure; `load_and_summarize` is the leak — it
reaches the filesystem through the adapter, so the policy must flag it."""

from __future__ import annotations

from effects import read_config, run_tool


def parse(text: str) -> dict[str, str]:
    out: dict[str, str] = {}
    for line in text.split("\n"):
        key, sep, value = line.partition("=")
        if sep:
            out[key.strip()] = value.strip()
    return out


def count_keys(config: dict[str, str]) -> int:
    return len(config)


def load_and_summarize(path_to_config: str) -> int:
    # Looks like plain core logic, but transitively performs filesystem I/O.
    config = parse(read_config(path_to_config))
    return count_keys(config)


def build_and_report(tool: str) -> str:
    # Transitively spawns a subprocess through the adapter.
    code = run_tool(tool)
    return f"{tool} exited {code}"
