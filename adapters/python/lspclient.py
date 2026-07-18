# A minimal stdio JSON-RPC LSP client, just enough to drive `ty server`
# (Astral's Rust type checker) over a project and ask it `textDocument/
# definition` at each call site. It speaks the Language Server Protocol's
# `Content-Length`-framed JSON-RPC on the server's stdin/stdout: send
# `initialize`/`initialized`, `textDocument/didOpen` every source file, then
# pipeline definition requests and collect the replies by id.
#
# It is intentionally tiny and dependency-free (standard library only) so the
# Python adapter's ty backend needs no package beyond the `ty` binary itself —
# the honest-capability contract is "ty present or fall back to Jedi," never a
# hidden third dependency.
from __future__ import annotations

import json
import subprocess
import threading
import time
from pathlib import Path


class LSP:
    """A stdio JSON-RPC client for one language server subprocess. A background
    reader thread demultiplexes responses (matched by id) from server-initiated
    notifications (diagnostics) and requests (answered with null so nothing
    blocks the server)."""

    def __init__(self, cmd, cwd=None):
        self.p = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=cwd,
            bufsize=0,
        )
        self._id = 0
        self._resp: dict[int, dict] = {}
        self._lock = threading.Lock()
        self._cv = threading.Condition(self._lock)
        self.diagnostics: dict[str, list] = {}
        self.stderr_lines: list[str] = []
        self._alive = True
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._reader.start()
        self._eread = threading.Thread(target=self._err_loop, daemon=True)
        self._eread.start()

    def _err_loop(self):
        for line in self.p.stderr:
            self.stderr_lines.append(line.decode("utf-8", "replace").rstrip())

    def _read_loop(self):
        stream = self.p.stdout
        while True:
            header = b""
            while b"\r\n\r\n" not in header:
                ch = stream.read(1)
                if not ch:
                    self._alive = False
                    with self._cv:
                        self._cv.notify_all()
                    return
                header += ch
            length = 0
            for h in header.split(b"\r\n"):
                if h.lower().startswith(b"content-length:"):
                    length = int(h.split(b":")[1].strip())
            body = b""
            while len(body) < length:
                chunk = stream.read(length - len(body))
                if not chunk:
                    self._alive = False
                    with self._cv:
                        self._cv.notify_all()
                    return
                body += chunk
            try:
                msg = json.loads(body.decode("utf-8"))
            except ValueError:
                continue
            if "id" in msg and ("result" in msg or "error" in msg):
                with self._cv:
                    self._resp[msg["id"]] = msg
                    self._cv.notify_all()
            elif msg.get("method") == "textDocument/publishDiagnostics":
                params = msg["params"]
                with self._cv:
                    self.diagnostics[params["uri"]] = params.get("diagnostics", [])
                    self._cv.notify_all()
            elif "id" in msg:
                # A server->client request: reply null so it does not block.
                self._send({"jsonrpc": "2.0", "id": msg["id"], "result": None})

    def _send(self, obj):
        data = json.dumps(obj).encode("utf-8")
        header = f"Content-Length: {len(data)}\r\n\r\n".encode("utf-8")
        self.p.stdin.write(header + data)
        self.p.stdin.flush()

    def notify(self, method, params):
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, method, params, timeout=15):
        rid = self.request_async(method, params)
        return self.wait(rid, timeout=timeout)

    def request_async(self, method, params) -> int:
        with self._lock:
            self._id += 1
            rid = self._id
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        return rid

    def wait(self, rid, timeout=15) -> dict:
        deadline = time.time() + timeout
        with self._cv:
            while rid not in self._resp:
                if not self._alive:
                    raise RuntimeError("the language server exited")
                remaining = deadline - time.time()
                if remaining <= 0:
                    raise TimeoutError(f"request {rid} timed out")
                self._cv.wait(remaining)
            return self._resp.pop(rid)

    def wait_for_diagnostics(self, timeout=8.0) -> bool:
        """Block until the server publishes diagnostics for at least one file, a
        signal that the first check pass has run and definitions will resolve.
        Returns True if diagnostics arrived, False on timeout (the caller then
        relies on retry-on-null). This is the settle step the spike flagged."""
        deadline = time.time() + timeout
        with self._cv:
            while not self.diagnostics:
                if not self._alive:
                    return False
                remaining = deadline - time.time()
                if remaining <= 0:
                    return False
                self._cv.wait(remaining)
            return True

    def shutdown(self):
        try:
            self.request("shutdown", None, timeout=5)
            self.notify("exit", None)
        except (RuntimeError, TimeoutError, OSError):
            pass
        try:
            self.p.terminate()
        except OSError:
            pass


def uri(path) -> str:
    return Path(path).resolve().as_uri()
