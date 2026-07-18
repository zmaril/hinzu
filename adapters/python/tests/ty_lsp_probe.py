#!/usr/bin/env python3
# A focused ty-LSP diagnostic: drive `ty server` over the fixture exactly as the
# adapter does, probe `textDocument/definition` for an imported-stdlib symbol
# (`subprocess.run`) and an ambient builtin (`open`), and dump the ty server's own
# stderr (under whatever `RUST_LOG` the caller set). It prints the raw definition
# TARGET URI for each, which is what pinned down the headless-runner behavior: ty
# resolves `open` to its vendored `builtins.pyi` but `subprocess.run` to the
# interpreter's real `.../lib/python3.11/subprocess.py` — a stdlib path the adapter
# now classifies as STDLIB. It keeps each CI run honest by showing exactly where ty
# resolves each symbol on that host.
#
# It never asserts and always exits 0 — it is a diagnostic, not a gate. Run it as
#   RUST_LOG=ty_server=debug,ty_ide=debug,ty_module_resolver=debug \
#     python3 ty_lsp_probe.py [fixture-dir]
from __future__ import annotations

import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))  # adapters/python
from lspclient import LSP, uri  # noqa: E402


def probe(root: str) -> None:
    py_version = f"{sys.version_info.major}.{sys.version_info.minor}"
    py_platform = sys.platform
    client = LSP(["ty", "server"], cwd=root)
    try:
        init = client.request("initialize", {
            "processId": os.getpid(),
            "rootUri": uri(root),
            "trace": "verbose",
            "initializationOptions": {
                "diagnosticMode": "workspace",
                "logLevel": "trace",
                "configuration": {"environment": {
                    "python-version": py_version, "python-platform": py_platform,
                }},
            },
            "capabilities": {"textDocument": {"definition": {"linkSupport": True}}},
            "workspaceFolders": [{"uri": uri(root), "name": "hinzu"}],
        }, timeout=30)
        caps = (init.get("result") or {}).get("capabilities", {})
        print(f"[probe] initialize ok | definitionProvider="
              f"{caps.get('definitionProvider')}", flush=True)
        client.notify("initialized", {})

        eff = os.path.join(root, "effects.py")
        src = Path(eff).read_text(encoding="utf-8")
        client.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri(eff), "languageId": "python", "version": 1, "text": src}})
        got_diags = client.wait_for_diagnostics(timeout=10.0)
        print(f"[probe] diagnostics published={got_diags}", flush=True)

        targets = []
        for lineno, line in enumerate(src.splitlines(), 1):
            if "subprocess.run" in line:
                targets.append(("subprocess.run", lineno,
                                line.index("subprocess.run") + len("subprocess.")))
            if "open(" in line and "def " not in line:
                targets.append(("open", lineno, line.index("open(")))

        for name, lineno, col in targets:
            resolved = None
            for attempt in range(6):
                try:
                    r = client.request("textDocument/definition", {
                        "textDocument": {"uri": uri(eff)},
                        "position": {"line": lineno - 1, "character": col},
                    }, timeout=15)
                except (RuntimeError, TimeoutError) as e:
                    print(f"[probe] {name}: request error {e}", flush=True)
                    break
                res = r.get("result")
                if res:
                    turi = res[0].get("targetUri") or res[0].get("uri")
                    resolved = turi
                    print(f"[probe] {name}: RESOLVED (attempt {attempt}) -> {turi}",
                          flush=True)
                    break
                time.sleep(0.5)
            if resolved is None:
                print(f"[probe] {name}: NULL after retries", flush=True)
    finally:
        client.shutdown()
        time.sleep(0.4)
        print("[probe] ---- ty server stderr ----", flush=True)
        for line in client.stderr_lines:
            print("[ty] " + line, flush=True)


def main() -> int:
    root = os.path.abspath(sys.argv[1] if len(sys.argv) > 1
                           else os.path.join(os.path.dirname(__file__), "fixture"))
    try:
        probe(root)
    except Exception as e:  # noqa: BLE001 — a diagnostic never fails the job
        print(f"[probe] error: {e}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
