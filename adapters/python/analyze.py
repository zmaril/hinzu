#!/usr/bin/env python3
# The hinzu Python adapter: a name-resolution-grade extractor that turns a Python
# project into hinzu's language-independent FactSet JSON.
#
# It is *extraction, not interpretation*. Walk every owned source file with the
# `ast` module keeping a stack of enclosing functions (the caller), and at each
# call site resolve the callee with Jedi's `goto(follow_imports=True)` — the
# closest maintained analogue to the TypeScript checker's getResolvedSignature.
# Effect roots are seeded by *declaration provenance*: Jedi tells us a callee's
# `full_name` and the module file it lives in, so `subprocess.run` resolves to
# the `subprocess` stdlib module and seeds a `process` root. Every effect name is
# a member of hinzu's ONE flat, shared vocabulary; Python seeds a subset (fs,
# net, process, env, clock, random) and never invents a Python-specific category.
# There is deliberately no `alloc` effect for Python.
#
# CRITICAL soundness rule (sound-by-default via Unknown): Python resolves only
# ~78% of call sites — duck-typed receivers, un-typed `pathlib` like
# `target.parent.mkdir`, `getattr`, decorators, dynamic import. An UNRESOLVED
# call site is emitted as an edge with `resolution: "unresolved"`, so hinzu-core
# turns it into an `Unknown` that FAILS CLOSED under the default `on_unknown =
# fail`. It is never silently dropped as pure — that is what keeps a
# weak-resolution language sound.
#
# The fact source is deliberately swappable behind this seam: Jedi today; a
# native-Rust type checker (pyrefly, then ty) is the planned future backend once
# one ships a stable library API, at higher fidelity through the same FactSet
# contract.
#
# Output (stdout) is exactly the schema `hinzu_core::FactSet::from_json` ingests:
#   { definitions: [...], edges: [...], effect_roots: [...] }
# All diagnostics go to stderr so stdout stays pure JSON.
#
# Usage: python3 analyze.py <project-dir>
from __future__ import annotations

import ast
import json
import sys
from pathlib import Path

try:
    import jedi
except ImportError:  # honest capability edge: no faked analysis without Jedi
    sys.stderr.write(
        "hinzu-py: the `jedi` package is required — run `pip install jedi` "
        "(see adapters/python/requirements.txt)\n"
    )
    sys.exit(3)

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

# os.path predicates that stat the filesystem. Jedi resolves `os.path` to the
# platform implementation module, so those spellings are covered too.
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
    on Jedi's full_name; the resolution order mirrors python.toml."""
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
        self.project_obj = jedi.Project(
            str(self.project), added_sys_path=[str(r) for r in self.source_roots]
        )
        self.definitions: dict[str, dict] = {}
        self.edges: list[dict] = []
        self.roots: dict[str, str] = {}
        self.owned_ids: set[str] = set()
        self._sources: dict[str, str] = {}
        self._scripts: dict[str, jedi.Script] = {}
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

    def script(self, path: str) -> jedi.Script:
        if path not in self._scripts:
            self._scripts[path] = jedi.Script(
                self._sources[path], path=path, project=self.project_obj
            )
        return self._scripts[path]

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
            Collector(self, str(f), self.rel(f), resolve=False).visit(tree)
        self.owned_ids = set(self.definitions)

    # ---- pass 2: resolve every call/reference site now that all owned ids
    # exist, so a forward reference between files still resolves to a local edge.
    def resolve(self):
        for f in self.owned_files():
            path = str(f)
            if path not in self._sources:
                continue
            tree = ast.parse(self._sources[path], filename=path)
            Collector(self, path, self.rel(f), resolve=True).visit(tree)

    # ---- pass 2: resolve callees + references with Jedi ---------------------
    def resolve_call(self, file: str, relpath: str, caller: str, func: ast.AST):
        self.n_call += 1
        line = func.lineno
        col = func.end_col_offset - 1
        try:
            defs = self.script(file).goto(
                line, col, follow_imports=True, follow_builtin_imports=True
            )
        except Exception:
            defs = []
        if not defs:
            # UNRESOLVED call site — fail closed. Emit an unknown-target edge so
            # hinzu-core turns it into an Unknown, never a silent pure.
            self.n_unresolved += 1
            try:
                text = ast.unparse(func)
            except Exception:
                text = "<call>"
            self.edges.append({
                "caller": caller,
                "callee": f"unresolved::{text}",
                "kind": "call",
                "resolution": "unresolved",
                "evidence_file": relpath,
                "evidence_line": line,
            })
            return

        d = defs[0]
        full_name = d.full_name
        mp = d.module_path
        # A call into a function we own: a plain call edge; its effects propagate
        # through its own body's edges.
        if self.is_owned(mp) and full_name:
            callee_id = self.local_id(mp, full_name)
            if callee_id and callee_id in self.owned_ids:
                self.n_resolved += 1
                self._edge(caller, callee_id, "call", relpath, line)
                return
            # Constructing an owned class: thread to its `__init__` so the
            # constructor's own effects (if any) propagate. A dataclass with no
            # explicit `__init__` has no tracked def, so construction is pure — no
            # edge, and never a false Unknown.
            if callee_id and d.type == "class":
                init_id = f"{callee_id}.__init__"
                self.n_resolved += 1
                if init_id in self.owned_ids:
                    self._edge(caller, init_id, "call", relpath, line)
                return
            # Resolved into an owned module but not a tracked function (a module
            # attribute / cached_property): fall through to external handling.

        if not full_name:
            # Resolved to something with no name (a parameter, a comprehension):
            # treat as unresolved so it fails closed rather than reading pure.
            self.n_unresolved += 1
            self.edges.append({
                "caller": caller, "callee": "unresolved::<unnamed>",
                "kind": "call", "resolution": "unresolved",
                "evidence_file": relpath, "evidence_line": line,
            })
            return

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

    def resolve_reference(self, file: str, relpath: str, caller: str, node: ast.AST):
        """A function value used without being called (a callback, a decorator, a
        default argument). Draw a reference edge ONLY to an owned definition or a
        known effect — a bare reference never manufactures an Unknown."""
        line = node.lineno
        col = node.end_col_offset - 1
        try:
            defs = self.script(file).goto(line, col, follow_imports=True)
        except Exception:
            return
        if not defs:
            return
        d = defs[0]
        # Reference edges track function *values* (callbacks, decorators), not
        # data attributes: a read of `proc.stderr` is not a subprocess effect just
        # because Jedi names its provenance in `subprocess`. Only a callable
        # resolution taints by reference.
        if d.type not in ("function", "method", "class"):
            return
        full_name, mp = d.full_name, d.module_path
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

    def seed_env(self, relpath: str, caller: str, env_node: ast.Attribute, file: str):
        """`os.environ` / `os.environb` read as an ambient value (the common
        `os.environ.get(...)` / `os.environ[...]` idiom). The `.get` itself
        resolves to a pure dict method, so the env effect is on the receiver —
        seed it here, confirmed against Jedi so a shadowed `os` never misfires."""
        try:
            defs = self.script(file).goto(env_node.lineno, env_node.end_col_offset - 1)
        except Exception:
            return
        full = defs[0].full_name if defs else None
        if not full or not full.startswith(("os.environ", "posix.environ")):
            return
        symbol = "os::environ"
        self._edge(caller, symbol, "reference", relpath, env_node.lineno)
        self.roots[symbol] = ENV
        self.n_ref += 1

    def _edge(self, caller, callee, kind, evfile, evline):
        if not caller or not callee or caller == callee:
            return
        self.edges.append({
            "caller": caller, "callee": callee, "kind": kind,
            "resolution": kind, "evidence_file": evfile, "evidence_line": evline,
        })

    def is_stdlib(self, module_path, full_name: str) -> bool:
        top = (full_name or "").split(".", 1)[0]
        if top == "builtins" or top in sys.stdlib_module_names:
            return True
        if top in ("posixpath", "ntpath", "genericpath", "pathlib"):
            return True
        if module_path is None:
            # A builtin / C-implemented callee Jedi could name but not locate.
            return bool(full_name)
        p = str(module_path).replace("\\", "/")
        return "/site-packages/" not in p and "/dist-packages/" not in p and (
            "/python3." in p or "/lib/python" in p
        )


class Collector(ast.NodeVisitor):
    """Walks a module keeping a stack of enclosing functions (the caller). With
    `resolve=False` (pass 1) it only registers function definitions; with
    `resolve=True` (pass 2) the definitions already exist and it emits the call
    and reference edges, attributed to the enclosing function."""

    def __init__(self, adapter: Adapter, file: str, relpath: str, resolve: bool):
        self.a = adapter
        self.file = file
        self.relpath = relpath
        self.resolve = resolve
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
        if not self.resolve:
            self.a.definitions[cid] = {
                "id": cid, "display": qual, "language": "python",
                "file": self.relpath, "line_start": node.lineno,
                "line_end": getattr(node, "end_lineno", node.lineno),
            }
        self.qual_stack.append(node.name)
        self.func_stack.append(cid)
        if self.resolve:
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
            self.a.resolve_reference(self.file, self.relpath, caller, target)
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
        if self.resolve:
            env = self._os_environ(node.value)
            if env is not None:
                self.a.seed_env(self.relpath, self._caller(), env, self.file)
        self.generic_visit(node)

    def visit_Call(self, node):
        if not self.resolve:
            self.generic_visit(node)
            return
        caller = self._caller()
        # `os.environ.get(...)` — the `.get` is a pure dict method; the env effect
        # is on the `os.environ` receiver, which the call walk does not descend
        # into, so catch it here.
        if isinstance(node.func, ast.Attribute):
            env = self._os_environ(node.func.value)
            if env is not None:
                self.a.seed_env(self.relpath, caller, env, self.file)
        self.a.resolve_call(self.file, self.relpath, caller, node.func)
        # A function value passed as an argument (a callback) is a reference.
        for arg in list(node.args) + [kw.value for kw in node.keywords]:
            if isinstance(arg, (ast.Name, ast.Attribute)):
                self.a.resolve_reference(self.file, self.relpath, caller, arg)
        # Descend into arguments (nested calls, lambdas) but not into `node.func`
        # again (already resolved as the call).
        for arg in node.args:
            self.visit(arg)
        for kw in node.keywords:
            self.visit(kw.value)


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
    sys.stderr.write(
        f"hinzu-py: jedi {jedi.__version__} | files {len(adapter.owned_files())} | "
        f"definitions {len(adapter.definitions)}\n"
    )
    adapter.resolve()

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
