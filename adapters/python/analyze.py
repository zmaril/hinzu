#!/usr/bin/env python3
# The hinzu Python adapter: a call-resolution extractor that turns a Python
# project into hinzu's language-independent FactSet JSON.
#
# It is *extraction, not interpretation*. Walk every owned source file with the
# standard-library `ast` module keeping a stack of enclosing functions (the
# caller) and record every call, function-value reference, and ambient-env read
# as a *site*. The resolution BACKEND then resolves each site's callee to a
# declaration, supplying the module + name provenance the effect roots key on.
# Effect roots are seeded by that declaration provenance: a callee resolving to
# `subprocess.run` seeds a `process` root, `pathlib.Path.mkdir` an `fs` root.
# Every effect name is a member of hinzu's ONE flat, shared vocabulary; Python
# seeds a subset (fs, net, process, env, clock, random) and never invents a
# Python-specific category. There is deliberately no `alloc` for a GC'd runtime.
#
# ONE BACKEND: ty. The AST walk, the caller attribution, the reference edges, and
# the whole owned/effect/stdlib/third-party classification are backend-independent
# — they run identically no matter what resolved a site. The resolution primitive
# is ty (Astral's Rust type checker), kept behind a seam so it can be swapped for a
# native in-process ty backend later:
#
#   * ty — driven over its LSP (`ty server`): `textDocument/definition` at each
#     callee token resolves the definition, whose target file (ty's vendored
#     typeshed, or an owned/third-party module) + enclosing qualname give the
#     provenance. A real type system resolves the un-typed `pathlib` receivers and
#     much of the duck-typed surface a name-resolver cannot, so they become precise
#     `fs`/`clock`/… edges. To make stdlib import resolution DETERMINISTIC on any
#     host (including headless CI runners, where ty's environment auto-discovery is
#     unreliable), the adapter pins ty's target `python-version` and
#     `python-platform` explicitly in the LSP `initialize` and warms ty's vendored
#     typeshed with a synchronous `ty check` before the batch, rather than relying
#     on ty to infer an environment. It is an LSP/subprocess upgrade today; the
#     intent is a native in-process ty backend behind this same seam once ty ships
#     a stable Rust library API.
#
# If the `ty` binary is absent the adapter exits nonzero with an honest message —
# it never falls back to a weaker resolver and never fakes a resolution.
#
# CRITICAL soundness rule (sound-by-default via Unknown): ty does not resolve
# every call site — duck-typed receivers, `getattr`, decorators, dynamic import.
# An UNRESOLVED call site is emitted as an edge with `resolution: "unresolved"`,
# so hinzu-core turns it into an `Unknown` that FAILS CLOSED under the default
# `on_unknown = fail`. It is never silently dropped as pure — that is what keeps
# a weak-resolution language sound.
#
# Output (stdout) is exactly the schema `hinzu_core::FactSet::from_json` ingests:
#   { definitions: [...], edges: [...], effect_roots: [...] }
# All diagnostics go to stderr so stdout stays pure JSON.
#
# Usage: python3 analyze.py <project-dir>
from __future__ import annotations

import ast
import json
import os
import re
import shutil
import sys
from pathlib import Path
from urllib.parse import unquote, urlparse

# --- shared, flat effect vocabulary (a subset of hinzu's categories) ----------
# The same names Rust and TypeScript use. A category that does not apply to
# Python simply does not appear — there is no `alloc` for a GC'd runtime.
FS, NET, PROCESS, ENV, CLOCK, RANDOM = "fs", "net", "process", "env", "clock", "random"

# Whole-module effects: any callee whose top module is one of these is that
# effect. `requests`/`httpx`/`urllib3`/`aiohttp` are the pragmatic well-known
# effectful third-party exception (like TypeScript's undici / execa).
EFFECT_MODULES = {
    "io": FS, "shutil": FS, "tempfile": FS, "glob": FS, "fileinput": FS,
    "linecache": FS,
    "socket": NET, "ssl": NET, "urllib": NET, "http": NET, "ftplib": NET,
    "smtplib": NET, "poplib": NET, "imaplib": NET, "telnetlib": NET,
    "xmlrpc": NET, "requests": NET, "httpx": NET, "urllib3": NET, "aiohttp": NET,
    "subprocess": PROCESS, "multiprocessing": PROCESS,
    "time": CLOCK,
    "random": RANDOM, "secrets": RANDOM,
}

# Specific dotted callees, resolved on the full_name. These win over the
# whole-module default and fan `os.*` out by operation (most of `os` is pure).
EFFECT_DOTTED = {
    "builtins.open": FS,
    "io.open": FS,
    # os filesystem
    "os.open": FS, "os.fdopen": FS, "os.remove": FS, "os.unlink": FS,
    "os.mkdir": FS, "os.makedirs": FS, "os.rmdir": FS, "os.removedirs": FS,
    "os.listdir": FS, "os.scandir": FS, "os.walk": FS, "os.stat": FS,
    "os.lstat": FS, "os.rename": FS, "os.renames": FS, "os.replace": FS,
    "os.link": FS, "os.symlink": FS, "os.readlink": FS, "os.chmod": FS,
    "os.chown": FS, "os.truncate": FS, "os.access": FS,
    # os process
    "os.system": PROCESS, "os.popen": PROCESS, "os.posix_spawn": PROCESS,
    "os.posix_spawnp": PROCESS, "os.spawnl": PROCESS, "os.spawnle": PROCESS,
    "os.spawnlp": PROCESS, "os.spawnlpe": PROCESS, "os.spawnv": PROCESS,
    "os.spawnve": PROCESS, "os.spawnvp": PROCESS, "os.spawnvpe": PROCESS,
    "os.execl": PROCESS, "os.execle": PROCESS, "os.execlp": PROCESS,
    "os.execlpe": PROCESS, "os.execv": PROCESS, "os.execve": PROCESS,
    "os.execvp": PROCESS, "os.execvpe": PROCESS, "os.fork": PROCESS,
    "os.forkpty": PROCESS, "os.kill": PROCESS, "os.abort": PROCESS,
    # os env
    "os.environ": ENV, "os.environb": ENV, "os.getenv": ENV, "os.getenvb": ENV,
    "os.putenv": ENV, "os.unsetenv": ENV, "os.getcwd": ENV, "os.getcwdb": ENV,
    "os.chdir": ENV,
    # clock (datetime is mostly pure; only the wall-clock reads count)
    "datetime.datetime.now": CLOCK, "datetime.datetime.today": CLOCK,
    "datetime.datetime.utcnow": CLOCK, "datetime.date.today": CLOCK,
}

# os.path predicates that stat the filesystem. ty resolves `os.path` to the
# platform implementation module (posixpath / ntpath / genericpath), so those
# spellings are covered too.
OS_PATH_FS = {
    "exists", "isfile", "isdir", "islink", "getsize", "getmtime", "getctime",
    "getatime", "realpath", "samefile",
}
OS_PATH_MODULES = ("os.path.", "posixpath.", "ntpath.", "genericpath.")

# pathlib I/O methods. The bare `pathlib.Path(...)` constructor and pure path
# algebra (joinpath, with_suffix, parent, name) are NOT effects — only the
# methods that touch the filesystem are `fs`.
PATHLIB_IO = {
    "open", "read_text", "write_text", "read_bytes", "write_bytes", "mkdir",
    "rmdir", "unlink", "touch", "rename", "replace", "symlink_to", "hardlink_to",
    "chmod", "lchmod", "stat", "lstat", "exists", "is_file", "is_dir",
    "is_symlink", "is_mount", "glob", "rglob", "iterdir", "walk", "resolve",
    "samefile", "expanduser", "owner", "group",
}

IGNORE_DIRS = {
    ".git", "__pycache__", ".venv", "venv", "env", ".env", "build", "dist",
    ".eggs", ".tox", ".nox", ".mypy_cache", ".pytest_cache", ".ruff_cache",
    "node_modules", "site-packages",
}


def effect_for(full_name: str) -> str | None:
    """The effect a resolved external callee seeds, or None if it is pure. Keyed
    on the callee's `full_name`; the resolution order mirrors python.toml. Keyed
    on the dotted name ty hands it, so it is backend-independent by construction."""
    if full_name in EFFECT_DOTTED:
        return EFFECT_DOTTED[full_name]
    # os.path / posixpath predicates that stat the filesystem.
    for prefix in OS_PATH_MODULES:
        if full_name.startswith(prefix) and full_name.rsplit(".", 1)[-1] in OS_PATH_FS:
            return FS
    # pathlib: only I/O methods. `pathlib._local` is the 3.13+ impl module.
    if full_name.startswith(("pathlib.", "pathlib._local.")):
        leaf = full_name.rsplit(".", 1)[-1]
        return FS if leaf in PATHLIB_IO else None
    top = full_name.split(".", 1)[0]
    if top == "os":
        return None  # os.path.join and friends are pure
    return EFFECT_MODULES.get(top)


def canonical_symbol(full_name: str) -> str:
    """A canonical `<module>::<rest.dotted>` symbol for an external callee, the
    same `::`-segmented shape Rust and TypeScript use, so python.toml and a
    project's `[roots]`/`[trust]` resolve it with the shared matcher.
    `subprocess.run` -> `subprocess::run`; `os.system` -> `os::system`."""
    full_name = full_name.replace("pathlib._local.", "pathlib.")
    top, _, rest = full_name.partition(".")
    return f"{top}::{rest}" if rest else top


class Resolution:
    """A backend-independent, normalized resolution of one call/reference site:
    the callee's dotted `full_name`, the filesystem `module_path` of the module
    that declares it (or None for a C builtin), and its `kind`
    (`function`/`method`/`class`/…). This is the seam a future native ty backend
    would produce too, so the classification downstream is one shared code path."""

    __slots__ = ("full_name", "module_path", "kind")

    def __init__(self, full_name, module_path, kind):
        self.full_name = full_name
        self.module_path = module_path
        self.kind = kind


class Site:
    """One place the AST walk found a callee/reference to resolve, attributed to
    its enclosing function. The walk records sites; a backend resolves them in a
    batch (ty pipelines them over LSP); then the shared classifier emits edges."""

    __slots__ = ("kind", "file", "relpath", "caller", "node", "line", "col")

    def __init__(self, kind, file, relpath, caller, node):
        self.kind = kind
        self.file = file
        self.relpath = relpath
        self.caller = caller
        self.node = node
        self.line = node.lineno
        self.col = node.end_col_offset - 1


def _uri_to_path(u: str) -> str:
    return unquote(urlparse(u).path)


class Adapter:
    def __init__(self, project: Path):
        self.project = project.resolve()
        # Source roots: a `src/` layout's `src` (so `import pkg` resolves) plus
        # the project root itself. Longest match wins when computing a file's
        # dotted module name.
        self.source_roots: list[Path] = []
        src = self.project / "src"
        if src.is_dir():
            self.source_roots.append(src)
        self.source_roots.append(self.project)
        self.definitions: dict[str, dict] = {}
        self.edges: list[dict] = []
        self.roots: dict[str, str] = {}
        self.owned_ids: set[str] = set()
        self.sites: list[Site] = []
        self._sources: dict[str, str] = {}
        self._qual_cache: dict[str, dict] = {}
        # counters (stderr diagnostics)
        self.n_call = self.n_resolved = self.n_unresolved = self.n_ref = 0

    # ---- file ownership + identity ------------------------------------------
    def owned_files(self) -> list[Path]:
        out = []
        for path in sorted(self.project.rglob("*.py")):
            if any(part in IGNORE_DIRS for part in path.relative_to(self.project).parts):
                continue
            out.append(path)
        return out

    def rel(self, path) -> str:
        try:
            return str(Path(path).resolve().relative_to(self.project)).replace("\\", "/")
        except ValueError:
            return str(path)

    def source_root_of(self, path: Path) -> Path:
        p = path.resolve()
        best = None
        for root in self.source_roots:
            try:
                p.relative_to(root)
            except ValueError:
                continue
            if best is None or len(str(root)) > len(str(best)):
                best = root
        return best or self.project

    def module_dotted(self, path) -> str:
        p = Path(path).resolve()
        root = self.source_root_of(p)
        rel = p.relative_to(root).with_suffix("")
        parts = [seg for seg in rel.parts if seg != "__init__"]
        return ".".join(parts)

    def is_owned(self, module_path) -> bool:
        if not module_path:
            return False
        p = Path(module_path).resolve()
        try:
            rel = p.relative_to(self.project)
        except ValueError:
            return False
        return not any(part in IGNORE_DIRS for part in rel.parts)

    def local_id(self, module_path, full_name: str) -> str | None:
        prefix = self.module_dotted(module_path) + "."
        qual = full_name[len(prefix):] if full_name and full_name.startswith(prefix) else full_name
        if not qual:
            return None
        return f"{self.rel(module_path)}#{qual}"

    def is_stdlib(self, module_path, full_name: str) -> bool:
        top = (full_name or "").split(".", 1)[0]
        if top == "builtins" or top in sys.stdlib_module_names:
            return True
        if top in ("posixpath", "ntpath", "genericpath", "pathlib"):
            return True
        if module_path is None:
            # A builtin / C-implemented callee named but not located.
            return bool(full_name)
        p = str(module_path).replace("\\", "/")
        if "/site-packages/" in p or "/dist-packages/" in p:
            return False
        return "/python3." in p or "/lib/python" in p or "/typeshed/" in p

    # ---- ty target-file provenance ------------------------------------------
    # These map a `textDocument/definition` target (a file path + line) to the
    # dotted module + qualname the classifier keys on. They must NOT hardcode
    # ty's vendored-typeshed hash directory — they detect the `/typeshed/…/
    # stdlib/` marker instead, so a ty version bump that changes the hash still
    # resolves. Detected at runtime from the actual target path ty returns.
    def module_of_target(self, path: str) -> tuple[str, str | None]:
        """Classify a definition target's file into (kind, dotted-module).
        kind is OWNED / STDLIB / THIRD_PARTY / OTHER."""
        try:
            rel = Path(path).resolve().relative_to(self.project)
            if not any(part in IGNORE_DIRS for part in rel.parts):
                return ("OWNED", self.module_dotted(path))
        except ValueError:
            pass
        p = path.replace("\\", "/")
        # ty's vendored typeshed: .../typeshed/<hash>/stdlib/<module>.pyi
        m = re.search(r"/typeshed/[^/]+/stdlib/(.+)\.pyi$", p)
        if m:
            return ("STDLIB", m.group(1).replace("/__init__", "").replace("/", "."))
        # ty's vendored typeshed third-party stubs: .../stubs/<dist>/<module>.pyi
        m = re.search(r"/typeshed/[^/]+/stubs/[^/]+/(.+)\.pyi$", p)
        if m:
            return ("THIRD_PARTY", m.group(1).replace("/__init__", "").replace("/", "."))
        # an installed third-party package (site-packages or dist-packages)
        m = re.search(r"/(?:site|dist)-packages/(.+)\.pyi?$", p)
        if m:
            return ("THIRD_PARTY", m.group(1).replace("/__init__", "").replace("/", "."))
        return ("OTHER", None)

    def qualname_at(self, path: str, line0: int) -> tuple[str | None, str | None]:
        """The dotted qualname and node kind of the def/class/assignment whose
        name sits on `line0` (0-based, as LSP reports it) in `path` — e.g.
        (`Path.mkdir`, `function`) or (`environ`, `statement`). Class-qualified
        so `datetime.datetime.now` and `datetime.date.today` stay distinct
        instead of collapsing to a bare `now` / `today`."""
        table = self._qual_cache.get(path)
        if table is None:
            table = {}
            src = self._sources.get(path)
            if src is None:
                try:
                    src = Path(path).read_text(encoding="utf-8", errors="replace")
                except OSError:
                    src = ""
            try:
                tree = ast.parse(src)
            except SyntaxError:
                tree = None
            if tree is not None:
                self._index_quals(tree, [], table)
            self._qual_cache[path] = table
        return table.get(line0 + 1, (None, None))

    def _index_quals(self, node, stack, table):
        for ch in ast.iter_child_nodes(node):
            if isinstance(ch, (ast.FunctionDef, ast.AsyncFunctionDef)):
                table[ch.lineno] = (".".join(stack + [ch.name]), "function")
                self._index_quals(ch, stack + [ch.name], table)
            elif isinstance(ch, ast.ClassDef):
                table[ch.lineno] = (".".join(stack + [ch.name]), "class")
                self._index_quals(ch, stack + [ch.name], table)
            else:
                # Module- and class-level bindings (e.g. `os.environ`) are the
                # provenance target for ambient values, not just callables.
                for name in self._assigned_names(ch):
                    table.setdefault(ch.lineno, (".".join(stack + [name]), "statement"))
                self._index_quals(ch, stack, table)

    @staticmethod
    def _assigned_names(node):
        if isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            return [node.target.id]
        if isinstance(node, ast.Assign):
            return [t.id for t in node.targets if isinstance(t, ast.Name)]
        return []

    # ---- pass 1: definitions only -------------------------------------------
    def collect(self):
        for f in self.owned_files():
            text = f.read_text(encoding="utf-8", errors="replace")
            self._sources[str(f)] = text
            try:
                tree = ast.parse(text, filename=str(f))
            except SyntaxError as e:
                sys.stderr.write(f"hinzu-py: skipping {self.rel(f)}: {e}\n")
                self._sources.pop(str(f), None)
                continue
            Collector(self, str(f), self.rel(f), record=False).visit(tree)
        self.owned_ids = set(self.definitions)

    # ---- pass 2: record every call/reference/env SITE (no resolution yet), so
    # a backend can resolve them all in one batch. Forward references between
    # files still resolve to a local edge because all owned ids already exist.
    def enumerate_sites(self):
        for f in self.owned_files():
            path = str(f)
            if path not in self._sources:
                continue
            tree = ast.parse(self._sources[path], filename=path)
            Collector(self, path, self.rel(f), record=True).visit(tree)

    def record(self, kind, file, relpath, caller, node):
        self.sites.append(Site(kind, file, relpath, caller, node))

    # ---- pass 3: resolve every site with the chosen backend, then emit edges
    # through the SHARED classifier — the same body no matter which backend ran.
    def resolve_and_emit(self, backend):
        queries = {(s.file, s.line, s.col) for s in self.sites}
        results = backend.resolve_batch(self._sources, queries)
        emit = {
            "call": self._emit_call,
            "reference": self._emit_reference,
            "env": self._emit_env,
        }
        for site in self.sites:
            emit[site.kind](site, results.get((site.file, site.line, site.col)))

    def _emit_call(self, site: Site, res: Resolution | None):
        self.n_call += 1
        caller, relpath, line = site.caller, site.relpath, site.line
        full_name = res.full_name if res else None
        if full_name is None:
            # UNRESOLVED (or resolved-but-unnamed) call site — fail closed. Emit
            # an unknown-target edge so hinzu-core turns it into an Unknown,
            # never a silent pure.
            self.n_unresolved += 1
            if res is None:
                try:
                    text = ast.unparse(site.node)
                except Exception:  # noqa: BLE001 — any unparse failure is a <call>
                    text = "<call>"
                callee = f"unresolved::{text}"
            else:
                callee = "unresolved::<unnamed>"
            self.edges.append({
                "caller": caller, "callee": callee, "kind": "call",
                "resolution": "unresolved", "evidence_file": relpath,
                "evidence_line": line,
            })
            return

        mp = res.module_path
        # A call into a function we own: a plain call edge; its effects propagate
        # through its own body's edges.
        if self.is_owned(mp):
            callee_id = self.local_id(mp, full_name)
            if callee_id and callee_id in self.owned_ids:
                self.n_resolved += 1
                self._edge(caller, callee_id, "call", relpath, line)
                return
            # Constructing an owned class: thread to its `__init__` so the
            # constructor's own effects (if any) propagate. A dataclass with no
            # explicit `__init__` has no tracked def, so construction is pure.
            if callee_id and res.kind == "class":
                self.n_resolved += 1
                init_id = f"{callee_id}.__init__"
                if init_id in self.owned_ids:
                    self._edge(caller, init_id, "call", relpath, line)
                return
            # Resolved into an owned module but not a tracked function (a module
            # attribute / cached_property): fall through to external handling.

        self.n_resolved += 1
        effect = effect_for(full_name)
        if effect:
            # An effectful stdlib / built-in / well-known package call: an effect
            # root, seeded directly by declaration provenance.
            symbol = canonical_symbol(full_name)
            self._edge(caller, symbol, "call", relpath, line)
            self.roots[symbol] = effect
            return
        if self.is_stdlib(mp, full_name):
            # A pure standard-library or built-in call: trusted pure, no edge, so
            # it never becomes an Unknown.
            return
        # A third-party package we cannot see through: an edge to a
        # `<package>::<member>` symbol with NO effect root, so hinzu-core marks it
        # Unknown and a policy can refuse to certify code reaching it, until a
        # `[trust]` line vouches for the package.
        self._edge(caller, canonical_symbol(full_name), "call", relpath, line)

    def _emit_reference(self, site: Site, res: Resolution | None):
        """A function value used without being called (a callback, a decorator, a
        default argument). Draw a reference edge ONLY to an owned definition or a
        known effect — a bare reference never manufactures an Unknown."""
        if res is None or res.kind not in ("function", "method", "class"):
            return
        caller, relpath, line = site.caller, site.relpath, site.line
        full_name, mp = res.full_name, res.module_path
        if self.is_owned(mp) and full_name:
            callee_id = self.local_id(mp, full_name)
            if callee_id and callee_id in self.owned_ids and callee_id != caller:
                self._edge(caller, callee_id, "reference", relpath, line)
                self.n_ref += 1
            return
        if full_name:
            effect = effect_for(full_name)
            if effect:
                symbol = canonical_symbol(full_name)
                self._edge(caller, symbol, "reference", relpath, line)
                self.roots[symbol] = effect
                self.n_ref += 1

    def _emit_env(self, site: Site, res: Resolution | None):
        """`os.environ` / `os.environb` read as an ambient value (the common
        `os.environ.get(...)` / `os.environ[...]` idiom). The `.get` itself
        resolves to a pure dict method, so the env effect is on the receiver —
        seed it here, confirmed against the backend so a shadowed `os` never
        misfires."""
        full = res.full_name if res else None
        if not full or not full.startswith(("os.environ", "posix.environ")):
            return
        symbol = "os::environ"
        self._edge(site.caller, symbol, "reference", site.relpath, site.line)
        self.roots[symbol] = ENV
        self.n_ref += 1

    def _edge(self, caller, callee, kind, evfile, evline):
        if not caller or not callee or caller == callee:
            return
        self.edges.append({
            "caller": caller, "callee": callee, "kind": kind,
            "resolution": kind, "evidence_file": evfile, "evidence_line": evline,
        })


class Collector(ast.NodeVisitor):
    """Walks a module keeping a stack of enclosing functions (the caller). With
    `record=False` (pass 1) it only registers function definitions; with
    `record=True` (pass 2) the definitions already exist and it RECORDS the call,
    reference, and ambient-env sites, attributed to the enclosing function, for a
    backend to resolve. The walk itself is backend-independent."""

    def __init__(self, adapter: Adapter, file: str, relpath: str, record: bool):
        self.a = adapter
        self.file = file
        self.relpath = relpath
        self.record = record
        self.qual_stack: list[str] = []
        self.func_stack: list[str] = []

    def _caller(self) -> str:
        return self.func_stack[-1] if self.func_stack else f"{self.relpath}#<module>"

    def visit_FunctionDef(self, node):
        self._function(node)

    def visit_AsyncFunctionDef(self, node):
        self._function(node)

    def _function(self, node):
        qual = ".".join(self.qual_stack + [node.name])
        cid = f"{self.relpath}#{qual}"
        if not self.record:
            self.a.definitions[cid] = {
                "id": cid, "display": qual, "language": "python",
                "file": self.relpath, "line_start": node.lineno,
                "line_end": getattr(node, "end_lineno", node.lineno),
            }
        self.qual_stack.append(node.name)
        self.func_stack.append(cid)
        if self.record:
            # A decorator is a reference to (or call of) a function value from the
            # enclosing scope — attribute it to this function.
            for dec in node.decorator_list:
                self._decorator(cid, dec)
        for child in node.body:
            self.visit(child)
        self.func_stack.pop()
        self.qual_stack.pop()

    def visit_ClassDef(self, node):
        self.qual_stack.append(node.name)
        for child in node.body:
            self.visit(child)
        self.qual_stack.pop()

    def _decorator(self, caller: str, dec: ast.AST):
        target = dec.func if isinstance(dec, ast.Call) else dec
        if isinstance(target, (ast.Name, ast.Attribute)):
            self.a.record("reference", self.file, self.relpath, caller, target)
        if isinstance(dec, ast.Call):
            self.visit(dec)

    @staticmethod
    def _os_environ(node: ast.AST) -> ast.Attribute | None:
        """The `os.environ` / `os.environb` attribute node, or None."""
        if (
            isinstance(node, ast.Attribute)
            and node.attr in ("environ", "environb")
            and isinstance(node.value, ast.Name)
            and node.value.id == "os"
        ):
            return node
        return None

    def visit_Subscript(self, node):
        # `os.environ["KEY"]` — an ambient env read (the receiver, not a call).
        if self.record:
            env = self._os_environ(node.value)
            if env is not None:
                self.a.record("env", self.file, self.relpath, self._caller(), env)
        self.generic_visit(node)

    def visit_Call(self, node):
        if not self.record:
            self.generic_visit(node)
            return
        caller = self._caller()
        # `os.environ.get(...)` — the `.get` is a pure dict method; the env effect
        # is on the `os.environ` receiver, which the call walk does not descend
        # into, so catch it here.
        if isinstance(node.func, ast.Attribute):
            env = self._os_environ(node.func.value)
            if env is not None:
                self.a.record("env", self.file, self.relpath, caller, env)
        self.a.record("call", self.file, self.relpath, caller, node.func)
        # A function value passed as an argument (a callback) is a reference.
        for arg in list(node.args) + [kw.value for kw in node.keywords]:
            if isinstance(arg, (ast.Name, ast.Attribute)):
                self.a.record("reference", self.file, self.relpath, caller, arg)
        # Descend into arguments (nested calls, lambdas) but not into `node.func`
        # again (already recorded as the call).
        for arg in node.args:
            self.visit(arg)
        for kw in node.keywords:
            self.visit(kw.value)


# ============================ resolution backends ============================
# Each backend turns a set of (file, line, col) query points into a dict of
# Resolution (or None for unresolved). Everything else — the walk, the edges,
# the effect classification — is shared and lives above.


class TyResolver:
    """The resolution backend: Astral's ty type checker over its LSP. Warms ty's
    vendored typeshed with a synchronous `ty check`, then opens every source file,
    waits for the first check pass to settle, then PIPELINES a
    `textDocument/definition` at each query point and maps the target
    (ty's vendored typeshed, or an owned/third-party module) to a Resolution.

    The target `python-version` and `python-platform` are pinned explicitly (to
    the running interpreter's) in both the `ty check` warm-up and the LSP
    `initialize`, so stdlib import resolution does not depend on ty's environment
    auto-discovery — which is unreliable on headless CI runners, where an
    un-pinned ty resolves `builtins` but returns null for imported-stdlib symbols
    like `subprocess.run`."""

    name = "ty"
    BATCH = 64

    def __init__(self, adapter: Adapter, ty_bin: str):
        self.a = adapter
        self.ty_bin = ty_bin
        self._version = "unknown"
        self._detect_version()
        # Pin ty's target to the interpreter actually running the adapter, so its
        # vendored-typeshed stdlib is selected deterministically on every host.
        self.py_version = f"{sys.version_info.major}.{sys.version_info.minor}"
        self.py_platform = sys.platform  # "linux" / "darwin" / "win32"

    def describe(self) -> str:
        return (
            f"ty {self._version} (LSP, python-version {self.py_version}, "
            f"platform {self.py_platform})"
        )

    @staticmethod
    def _to_utf16(source: str, line1: int, bytecol: int) -> int:
        """LSP positions are utf-16 code units; ast columns are utf-8 byte
        offsets. Convert one column on one line."""
        try:
            line = source.splitlines()[line1 - 1]
        except IndexError:
            return bytecol
        prefix = line.encode("utf-8")[:bytecol].decode("utf-8", "replace")
        return len(prefix.encode("utf-16-le")) // 2

    def _detect_version(self):
        import subprocess  # noqa: PLC0415 — only when the ty backend runs

        try:
            out = subprocess.run(
                [self.ty_bin, "--version"], capture_output=True, text=True, timeout=10
            )
            self._version = (out.stdout or out.stderr).strip().split()[-1]
        except (OSError, subprocess.SubprocessError, IndexError):
            pass

    def _warm_check(self, root: str):
        """Warm ty's vendored typeshed and build the module graph DETERMINISTICALLY
        with a synchronous `ty check` before the LSP batch. `ty check` resolves
        every import to completion and materializes the vendored typeshed stubs
        into ty's shared cache — the same cache the LSP server then serves
        `textDocument/definition` from — so the batch never races an async,
        half-warm index. Pinning `--python-version`/`--python-platform` to the same
        values the LSP uses keeps the two in agreement. Best-effort: a nonzero exit
        (a project with type errors is normal and expected) does not stop the run;
        the resolution comes from the LSP, this only warms and diagnoses."""
        import subprocess  # noqa: PLC0415 — only when the ty backend runs

        cmd = [
            self.ty_bin, "check", "--project", root,
            "--python-version", self.py_version,
            "--python-platform", self.py_platform,
            "--exit-zero", "--output-format", "concise",
        ]
        try:
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
            tail = (out.stderr or out.stdout or "").strip().splitlines()
            sys.stderr.write(
                f"hinzu-py: ty check warm-up exit {out.returncode}"
                + (f" | {tail[-1]}" if tail else "")
                + "\n"
            )
        except (OSError, subprocess.SubprocessError) as e:
            sys.stderr.write(f"hinzu-py: ty check warm-up skipped ({e})\n")

    def resolve_batch(self, sources, queries):
        from lspclient import LSP, uri  # noqa: PLC0415 — only when ty is chosen

        root = str(self.a.project)
        self._warm_check(root)
        client = LSP([self.ty_bin, "server"], cwd=root)
        try:
            client.request("initialize", {
                "processId": os.getpid(),
                "rootUri": uri(root),
                # Pin ty's target environment so imported-stdlib resolution does
                # not depend on ty's environment auto-discovery (unreliable on
                # headless runners). `diagnosticMode: workspace` makes ty index the
                # whole project, not just open files, so cross-module definitions
                # settle. These options are ty's LSP `initializationOptions`
                # (GlobalOptions), passed at the top level — NOT nested under a
                # `settings` key, which ty rejects as "unknown options".
                "initializationOptions": {
                    "diagnosticMode": "workspace",
                    "configuration": {"environment": {
                        "python-version": self.py_version,
                        "python-platform": self.py_platform,
                    }},
                },
                "capabilities": {"textDocument": {
                    "definition": {"linkSupport": True},
                    "hover": {"contentFormat": ["plaintext", "markdown"]},
                }},
                "workspaceFolders": [{"uri": uri(root), "name": "hinzu"}],
            }, timeout=30)
            client.notify("initialized", {})
            # Open every owned source file so cross-file definitions resolve.
            for path, src in sources.items():
                client.notify("textDocument/didOpen", {"textDocument": {
                    "uri": uri(path), "languageId": "python", "version": 1,
                    "text": src}})
            # Settle. On a cold run ty resolves owned symbols and `builtins`
            # before its vendored typeshed is warm for other stdlib modules
            # (`subprocess`, …), so a batch fired too early sees those as null —
            # the first-run race the spikes flagged. Wait until a stdlib
            # definition actually resolves, using an in-memory probe doc, before
            # asking anything. Cheap once warm (resolves on the first poll).
            client.wait_for_diagnostics(timeout=8.0)
            self._await_ready(client, uri, root)

            qlist = list(queries)
            raw = self._pipeline(client, uri, sources, qlist)
            # Retry-on-null with backoff: a null early on may just mean the
            # workspace had not finished checking that file, or ty answered a
            # request inconsistently under load. Re-fire only the still-null set,
            # a few times with a short sleep between passes, stopping as soon as a
            # pass reclaims nothing. Genuine unresolvables stay null, so this is a
            # bounded handful of extra passes, not per-site polling.
            import time  # noqa: PLC0415 — only when the ty backend runs

            for delay in (0.3, 0.6, 1.2, 2.0):
                nulls = [q for q in qlist if raw.get(q) is None]
                if not nulls:
                    break
                time.sleep(delay)
                reclaimed = 0
                for q, v in self._pipeline(client, uri, sources, nulls).items():
                    if v is not None:
                        raw[q] = v
                        reclaimed += 1
                if reclaimed == 0:
                    break
            return {q: self._resolution(raw.get(q)) for q in qlist}
        finally:
            client.shutdown()

    def _await_ready(self, client, uri, root, timeout=25.0):
        """Block until ty can resolve a stdlib definition, so the real batch does
        not race the cold-start warm-up. Opens a throwaway in-memory doc that
        references `subprocess.run` (a call the fixtures and most projects use)
        and polls its definition until it lands in typeshed or `timeout` elapses.
        The probe doc is never written to disk; it is closed afterwards."""
        import time  # noqa: PLC0415 — only when the ty backend runs

        probe = os.path.join(root, "__hinzu_ty_ready__.py")
        client.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri(probe), "languageId": "python", "version": 1,
            "text": "import subprocess\nsubprocess.run\n"}})
        deadline = time.time() + timeout
        try:
            while time.time() < deadline:
                try:
                    result = client.request("textDocument/definition", {
                        "textDocument": {"uri": uri(probe)},
                        "position": {"line": 1, "character": 13}}, timeout=15).get("result")
                except (RuntimeError, TimeoutError):
                    result = None
                if result:
                    turi = result[0].get("targetUri") or result[0].get("uri") or ""
                    if "/typeshed/" in turi or "subprocess" in turi:
                        return
                time.sleep(0.2)
        finally:
            client.notify("textDocument/didClose",
                          {"textDocument": {"uri": uri(probe)}})

    def _pipeline(self, client, uri, sources, qlist):
        """Fire definition requests in windows of BATCH, then collect — the
        request pipelining that keeps the whole project under ~2s."""
        raw = {}
        i = 0
        while i < len(qlist):
            window = qlist[i:i + self.BATCH]
            pending = []
            for key in window:
                file, line, col = key
                char = self._to_utf16(sources[file], line, col)
                rid = client.request_async("textDocument/definition", {
                    "textDocument": {"uri": uri(file)},
                    "position": {"line": line - 1, "character": char}})
                pending.append((key, rid))
            for key, rid in pending:
                try:
                    raw[key] = client.wait(rid, timeout=30).get("result")
                except (RuntimeError, TimeoutError):
                    raw[key] = None
            i += self.BATCH
        return raw

    def _resolution(self, result) -> Resolution | None:
        # `result` is a list of LocationLink / Location, or null/empty.
        if not result:
            return None
        first = result[0]
        turi = first.get("targetUri") or first.get("uri")
        trange = first.get("targetSelectionRange") or first.get("range")
        if not turi or not trange:
            return None
        tpath = _uri_to_path(turi)
        line0 = trange["start"]["line"]
        kind, mod = self.a.module_of_target(tpath)
        qual, ntype = self.a.qualname_at(tpath, line0)
        if kind == "OWNED":
            if qual is None:
                # Owned target we cannot pin to a tracked def (a variable holding
                # a callable): unresolved, fail closed, never a fake symbol.
                return None
            return Resolution(f"{self.a.module_dotted(tpath)}.{qual}", tpath, ntype)
        if kind in ("STDLIB", "THIRD_PARTY"):
            full_name = f"{mod}.{qual}" if qual else mod
            return Resolution(full_name, tpath, ntype)
        return None  # OTHER / unmappable target: fail closed


def choose_backend(adapter: Adapter):
    """Build the ty resolution backend, the adapter's SOLE backend. ty's binary
    must be present (`HINZU_TY` overrides the path); if it is absent the adapter
    exits nonzero with an honest message. There is no fallback resolver — an
    honest capability edge, never a faked or weaker resolution."""
    ty_bin = os.environ.get("HINZU_TY") or shutil.which("ty")
    if not ty_bin:
        sys.stderr.write(
            "hinzu-py: the `ty` binary was not found — ty is the adapter's sole "
            "resolution backend. Install it (`uv tool install ty` or "
            "`pip install ty`) or set HINZU_TY to its path.\n"
        )
        sys.exit(3)
    return TyResolver(adapter, ty_bin)


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        sys.stderr.write("usage: python3 analyze.py <project-dir>\n")
        return 2
    project = Path(argv[1])
    if not project.is_dir():
        sys.stderr.write(f"hinzu-py: {project} is not a directory\n")
        return 2

    adapter = Adapter(project)
    adapter.collect()
    backend = choose_backend(adapter)
    sys.stderr.write(
        f"hinzu-py: backend {backend.describe()} | files "
        f"{len(adapter.owned_files())} | definitions {len(adapter.definitions)}\n"
    )
    adapter.enumerate_sites()
    adapter.resolve_and_emit(backend)

    resolved_pct = 100 * adapter.n_resolved / max(1, adapter.n_call)
    sys.stderr.write(
        f"hinzu-py: call sites {adapter.n_call} | resolved {adapter.n_resolved} "
        f"({resolved_pct:.1f}%) | unresolved {adapter.n_unresolved} | "
        f"reference {adapter.n_ref} | effect roots {len(adapter.roots)}\n"
    )

    out = {
        "definitions": list(adapter.definitions.values()),
        "edges": adapter.edges,
        "effect_roots": [
            {"symbol": s, "effect": e} for s, e in sorted(adapter.roots.items())
        ],
    }
    sys.stdout.write(json.dumps(out, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
