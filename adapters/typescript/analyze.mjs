// The hinzu TypeScript adapter: a native TypeScript compiler-API extractor that
// turns a TS project into hinzu's language-independent FactSet JSON.
//
// It is *extraction, not interpretation*. Build one Program from the project's
// tsconfig, walk every source file keeping a stack of enclosing functions (the
// caller), and at each call-like node use `checker.getResolvedSignature()` to
// reach the callee's declaration. Effect roots are seeded by *declaration
// provenance* — the checker tells us a callee's declaration lives in
// `@types/node/fs.d.ts` or in `lib.dom.d.ts` (the ambient `fetch`), which is the
// only sound way to seed roots that survive aliasing and re-export. Every effect
// name is a member of hinzu's ONE flat, shared vocabulary; TypeScript seeds a
// subset (fs, net, process, env, clock, random) and never invents a TS-specific
// category. There is deliberately no `alloc` effect for TypeScript.
//
// Output (stdout) is exactly the schema `hinzu_core::FactSet::from_json` ingests:
//   { definitions: [...], edges: [...], effect_roots: [...] }
// All diagnostics go to stderr so stdout stays pure JSON.
//
// Usage: node analyze.mjs <project-dir> [--tsconfig <path>]

import ts from "typescript";
import path from "node:path";
import fs from "node:fs";

// --- shared, flat effect vocabulary (a subset of hinzu's categories) ---------
// The same names Rust uses. A category that does not apply to TypeScript simply
// does not appear here — there is no `alloc` for a GC'd runtime.
const FS = "fs";
const NET = "net";
const PROCESS = "process";
const ENV = "env";
const CLOCK = "clock";
const RANDOM = "random";

// Node built-in modules whose whole surface is one effect. Keyed by the module
// basename the checker resolves a callee's declaration file to.
const EFFECTFUL_NODE_MODULES = {
  fs: FS,
  "fs/promises": FS,
  child_process: PROCESS,
  net: NET,
  http: NET,
  https: NET,
  http2: NET,
  tls: NET,
  dgram: NET,
  dns: NET,
};

// The crypto members that produce randomness (the rest of `node:crypto` — hashes,
// ciphers — is not a certifiable effect here, so it is left pure).
const CRYPTO_RANDOM = new Set([
  "getRandomValues",
  "randomBytes",
  "randomUUID",
  "randomInt",
  "randomFillSync",
  "randomFill",
  "generateKeyPair",
  "generateKeyPairSync",
]);

// Well-known effectful npm packages the ecosystem treats as I/O primitives. These
// are part of the shared seed vocabulary (the catalog lists them), so a call into
// one is an effect root, not an Unknown. Every *other* third-party package stays
// Unknown until a project vouches for it via `[trust]` in hinzu.toml.
const EFFECTFUL_NPM = {
  "cross-spawn": PROCESS,
  execa: PROCESS,
  undici: NET,
  "node-fetch": NET,
  ws: NET,
};

// --- argument parsing --------------------------------------------------------
const argv = process.argv.slice(2);
let projectArg = null;
let tsconfigArg = null;
for (let i = 0; i < argv.length; i++) {
  if (argv[i] === "--tsconfig") tsconfigArg = argv[++i];
  else if (!projectArg) projectArg = argv[i];
}
if (!projectArg) {
  console.error("usage: node analyze.mjs <project-dir> [--tsconfig <path>]");
  process.exit(2);
}
const ROOT = path.resolve(projectArg);
const tsconfigPath = tsconfigArg
  ? path.resolve(tsconfigArg)
  : ts.findConfigFile(ROOT, ts.sys.fileExists, "tsconfig.json");
if (!tsconfigPath || !fs.existsSync(tsconfigPath)) {
  console.error(`no tsconfig.json found under ${ROOT} — pass --tsconfig <path>`);
  process.exit(2);
}

// --- build the Program from the project's own tsconfig -----------------------
const cfgFile = ts.readConfigFile(tsconfigPath, ts.sys.readFile);
const parsed = ts.parseJsonConfigFileContent(
  cfgFile.config,
  ts.sys,
  path.dirname(tsconfigPath),
  { noEmit: true },
);
if (parsed.errors.length) {
  for (const e of parsed.errors) {
    console.error("tsconfig:", ts.flattenDiagnosticMessageText(e.messageText, "\n"));
  }
}
const program = ts.createProgram({ rootNames: parsed.fileNames, options: parsed.options });
const checker = program.getTypeChecker();
console.error(
  `hinzu-ts: TypeScript ${ts.version} | root files ${parsed.fileNames.length} | ` +
    `program sources ${program.getSourceFiles().length}`,
);

// --- which files we own (attribute definitions to) ---------------------------
// Everything under the project root that is real source: not a dependency, not a
// declaration file, not build output. Tests are kept as definitions; a policy's
// `ignore` globs, not the adapter, decide whether to skip them.
const IGNORED_DIRS = /(^|\/)(node_modules|dist|build|out|coverage|\.git)(\/|$)/;
function ownedRel(fileName) {
  const rel = path.relative(ROOT, fileName);
  if (!rel || rel.startsWith("..") || path.isAbsolute(rel)) return null;
  const unix = rel.split(path.sep).join("/");
  if (IGNORED_DIRS.test("/" + unix)) return null;
  if (unix.endsWith(".d.ts")) return null;
  return unix;
}

// --- symbol ids --------------------------------------------------------------
// A local callable's id is its file (no extension) plus its qualified name, made
// unique per file. External (no-body) callees get a `::`-segmented id so the same
// `[roots]`/`[trust]` matcher Rust uses resolves them: `node:fs::readFileSync`,
// `global::fetch`, or `<package>::<member>` for an npm call.
function nodeBuiltinSymbol(moduleName, member) {
  return `node:${moduleName}::${member}`;
}
function globalSymbol(name) {
  return `global::${name}`;
}
function npmSymbol(pkg, member) {
  return `${pkg}::${member}`;
}

// --- definitions: one per function-like node in an owned file ----------------
const defIdByNode = new Map(); // ts.Node -> id
const defs = new Map(); // id -> definition record
let anon = 0;

function isFunctionLike(n) {
  return (
    ts.isFunctionDeclaration(n) ||
    ts.isMethodDeclaration(n) ||
    ts.isArrowFunction(n) ||
    ts.isFunctionExpression(n) ||
    ts.isConstructorDeclaration(n) ||
    ts.isGetAccessorDeclaration(n) ||
    ts.isSetAccessorDeclaration(n)
  );
}

function nameForNode(n) {
  if (
    (ts.isFunctionDeclaration(n) ||
      ts.isMethodDeclaration(n) ||
      ts.isGetAccessorDeclaration(n) ||
      ts.isSetAccessorDeclaration(n)) &&
    n.name
  ) {
    return n.name.getText();
  }
  if (ts.isConstructorDeclaration(n)) return "constructor";
  const p = n.parent;
  if (p && ts.isVariableDeclaration(p) && p.name) return p.name.getText();
  if (p && ts.isPropertyAssignment(p) && p.name) return p.name.getText();
  if (p && ts.isPropertyDeclaration(p) && p.name) return p.name.getText();
  if (p && ts.isExportAssignment(p)) return "(default)";
  if (p && (ts.isCallExpression(p) || ts.isNewExpression(p))) return "(callback)";
  return "(anonymous)";
}

function qualifierChain(n) {
  const parts = [];
  let cur = n.parent;
  while (cur) {
    if (ts.isClassDeclaration(cur) || ts.isClassExpression(cur)) {
      parts.unshift(cur.name ? cur.name.getText() : "(class)");
    } else if (ts.isModuleDeclaration(cur) && cur.name) {
      parts.unshift(cur.name.getText());
    }
    cur = cur.parent;
  }
  return parts;
}

function lineOf(sf, pos) {
  return sf.getLineAndCharacterOfPosition(pos).line + 1;
}

for (const sf of program.getSourceFiles()) {
  const rel = ownedRel(sf.fileName);
  if (!rel) continue;
  const relNoExt = rel.replace(/\.[cm]?tsx?$/, "");
  const walk = (n) => {
    if (isFunctionLike(n)) {
      const qualified = [...qualifierChain(n), nameForNode(n)].filter(Boolean).join(".");
      let id = `${relNoExt}#${qualified}`;
      if (defs.has(id)) id += `@${lineOf(sf, n.getStart())}`;
      if (defs.has(id)) id += `~${anon++}`;
      defIdByNode.set(n, id);
      defs.set(id, {
        id,
        display: qualified,
        language: "typescript",
        file: rel,
        line_start: lineOf(sf, n.getStart()),
        line_end: lineOf(sf, n.getEnd()),
      });
    }
    ts.forEachChild(n, walk);
  };
  ts.forEachChild(sf, walk);
}
console.error(`hinzu-ts: definitions ${defs.size}`);

// --- edges + effect roots ----------------------------------------------------
const edges = []; // {caller, callee, kind, resolution, evidence_file, evidence_line}
const rootSet = new Map(); // symbol -> effect (deduped effect roots)

function addEdge(caller, callee, kind, evFile, evLine) {
  if (!caller || !callee || caller === callee) return;
  edges.push({
    caller,
    callee,
    kind,
    resolution: kind,
    evidence_file: evFile,
    evidence_line: evLine,
  });
}
function addEffectLeaf(caller, symbol, effect, evFile, evLine) {
  addEdge(caller, symbol, "call", evFile, evLine);
  rootSet.set(symbol, effect);
}

const LIB_RE = /\/lib\.[^/]+\.d\.ts$/;
const NODE_TYPES_RE = /@types\/node\/([^.][^/]*(?:\/[^.][^/]*)*)\.d\.ts$/;

function declFilesOfSymbol(sym) {
  const out = [];
  for (const d of sym?.getDeclarations?.() || []) out.push(d.getSourceFile().fileName);
  return out;
}

// Resolve a call/new/tagged-template to its callee declaration node, file, and
// symbol — the checker doing the heavy lifting (typed receivers, aliases,
// re-exports, ambient globals).
function resolveCallee(node) {
  let sig = null;
  try {
    sig = checker.getResolvedSignature(node);
  } catch {}
  let declNode = sig?.getDeclaration ? sig.getDeclaration() : undefined;
  const exprForSym = ts.isTaggedTemplateExpression(node) ? node.tag : node.expression;
  let sym = exprForSym ? checker.getSymbolAtLocation(exprForSym) : undefined;
  if (sym && sym.flags & ts.SymbolFlags.Alias) {
    try {
      sym = checker.getAliasedSymbol(sym);
    } catch {}
  }
  if (!declNode && sym) {
    const ds = sym.getDeclarations?.() || [];
    declNode = ds.find(isFunctionLike) || ds[0];
  }
  const declFile = declNode
    ? declNode.getSourceFile().fileName
    : sym
      ? declFilesOfSymbol(sym)[0]
      : undefined;
  return { declNode, declFile };
}

function calleeMember(node) {
  const e = ts.isTaggedTemplateExpression(node) ? node.tag : node.expression;
  if (!e) return "call";
  if (ts.isIdentifier(e)) return e.text;
  if (ts.isPropertyAccessExpression(e)) return e.name.text;
  return e.getText().slice(0, 40);
}

// A node built-in effect from the callee's declaration file, or null.
function nodeBuiltinEffect(declFile, member) {
  if (!declFile) return null;
  const m = declFile.match(NODE_TYPES_RE);
  if (!m) return null;
  const moduleName = m[1];
  if (moduleName === "crypto" || moduleName === "crypto/promises") {
    return CRYPTO_RANDOM.has(member)
      ? { symbol: nodeBuiltinSymbol("crypto", member), effect: RANDOM }
      : null;
  }
  const effect = EFFECTFUL_NODE_MODULES[moduleName];
  if (!effect) return null;
  return { symbol: nodeBuiltinSymbol(moduleName, member), effect };
}

// Per-file import map: local name -> { pkg, effect|null }. `effect` is set for a
// known effectful npm package; a bare import of any other package is recorded
// with `effect: null` so an unresolved call through it becomes an Unknown.
function buildImportMap(sf) {
  const map = new Map();
  const record = (name, pkg) => map.set(name, { pkg, effect: EFFECTFUL_NPM[pkg] ?? null });
  for (const st of sf.statements) {
    if (!ts.isImportDeclaration(st) || !ts.isStringLiteral(st.moduleSpecifier)) continue;
    const spec = st.moduleSpecifier.text;
    if (spec.startsWith(".") || spec.startsWith("node:")) continue; // local / node builtin
    const pkg = packageOfSpecifier(spec);
    const ic = st.importClause;
    if (!ic) continue;
    if (ic.name) record(ic.name.text, pkg);
    const nb = ic.namedBindings;
    if (nb && ts.isNamespaceImport(nb)) record(nb.name.text, pkg);
    else if (nb) for (const el of nb.elements) record(el.name.text, pkg);
  }
  return map;
}

// The package name of a bare import specifier: `@scope/name/sub` -> `@scope/name`,
// `pkg/sub` -> `pkg`.
function packageOfSpecifier(spec) {
  const parts = spec.split("/");
  return spec.startsWith("@") ? parts.slice(0, 2).join("/") : parts[0];
}

function rootIdentifier(expr) {
  let e = expr;
  while (e && ts.isPropertyAccessExpression(e)) e = e.expression;
  return e && ts.isIdentifier(e) ? e : null;
}

// Effectful ambient globals reached by property access or bare identifier —
// `process.env`, `Date.now`, `Math.random`, `fetch`. Confirmed against the
// declaration file (lib.*.d.ts / @types/node) so a user object of the same name
// never misfires.
function symFromLibOrNode(node) {
  return declFilesOfSymbol(checker.getSymbolAtLocation(node)).some(
    (f) => LIB_RE.test(f) || /@types\/node\//.test(f),
  );
}
const GLOBAL_MEMBER_EFFECTS = {
  process: { env: ENV, argv: ENV, argv0: ENV, cwd: ENV, exit: ENV },
  Date: { now: CLOCK },
  performance: { now: CLOCK },
  Math: { random: RANDOM },
};
function classifyGlobalAccess(node) {
  if (!ts.isPropertyAccessExpression(node) || !ts.isIdentifier(node.expression)) return null;
  const obj = node.expression.text;
  const effect = GLOBAL_MEMBER_EFFECTS[obj]?.[node.name.text];
  if (!effect || !symFromLibOrNode(node.expression)) return null;
  return { symbol: globalSymbol(`${obj}.${node.name.text}`), effect };
}
const BARE_CALL_EFFECTS = { fetch: NET, WebSocket: NET };
function classifyBareCall(node) {
  const e = node.expression;
  if (!e || !ts.isIdentifier(e)) return null;
  const effect = BARE_CALL_EFFECTS[e.text];
  if (!effect || !symFromLibOrNode(e)) return null;
  return { symbol: globalSymbol(e.text), effect };
}

// Should a bare identifier reference (not a call) draw a reference edge to a
// local definition? Skips declaration names, property names, and import/export
// specifiers — only a genuine *use* of a function value taints.
function isReferenceUse(id) {
  const p = id.parent;
  if (!p) return false;
  if ((ts.isCallExpression(p) || ts.isNewExpression(p)) && p.expression === id) return false;
  if (ts.isPropertyAccessExpression(p) && p.name === id) return false;
  if (ts.isImportSpecifier(p) || ts.isExportSpecifier(p) || ts.isImportClause(p)) return false;
  const isDeclName =
    (ts.isFunctionDeclaration(p) ||
      ts.isMethodDeclaration(p) ||
      ts.isVariableDeclaration(p) ||
      ts.isParameter(p) ||
      ts.isPropertyAssignment(p) ||
      ts.isBindingElement(p)) &&
    p.name === id;
  return !isDeclName;
}

let callSites = 0;
let resolved = 0;
let refEdges = 0;
let unknownEdges = 0;

for (const sf of program.getSourceFiles()) {
  const rel = ownedRel(sf.fileName);
  if (!rel) continue;
  const importMap = buildImportMap(sf);
  const stack = [];
  const walk = (n) => {
    const isFn = isFunctionLike(n);
    if (isFn) stack.push(defIdByNode.get(n));
    const enclosing = stack[stack.length - 1];

    if (enclosing) {
      const line = lineOf(sf, n.getStart());

      // Ambient global effect reached by property access (process.env, Date.now).
      const ga = classifyGlobalAccess(n);
      if (ga) addEffectLeaf(enclosing, ga.symbol, ga.effect, rel, line);

      if (ts.isCallExpression(n) || ts.isNewExpression(n) || ts.isTaggedTemplateExpression(n)) {
        callSites++;
        handleCall(n, enclosing, rel, line, importMap);
      }

      // Reference-level taint: a bare identifier resolving to a local function,
      // used as a value (callback, default parameter) rather than called.
      if (ts.isIdentifier(n) && isReferenceUse(n)) {
        let s = checker.getSymbolAtLocation(n);
        if (s && s.flags & ts.SymbolFlags.Alias) {
          try {
            s = checker.getAliasedSymbol(s);
          } catch {}
        }
        const fnDecl = (s?.getDeclarations?.() || []).find(isFunctionLike);
        const calleeId = fnDecl && defIdByNode.get(fnDecl);
        if (calleeId && calleeId !== enclosing) {
          addEdge(enclosing, calleeId, "reference", rel, line);
          refEdges++;
        }
      }
    }

    ts.forEachChild(n, walk);
    if (isFn) stack.pop();
  };
  ts.forEachChild(sf, walk);
}

function handleCall(n, enclosing, rel, line, importMap) {
  const { declNode, declFile } = resolveCallee(n);

  // 1. A call into a function we own: a plain call edge; its effects propagate
  //    through its own body's edges.
  const localId = declNode && defIdByNode.get(declNode);
  if (localId) {
    resolved++;
    addEdge(enclosing, localId, "call", rel, line);
    return;
  }

  const member = calleeMember(n);

  // 2. An effectful node built-in or ambient global — an effect root.
  const builtin =
    nodeBuiltinEffect(declFile, member) ||
    (ts.isCallExpression(n) ? classifyBareCall(n) : null);
  if (builtin) {
    resolved++;
    addEffectLeaf(enclosing, builtin.symbol, builtin.effect, rel, line);
    return;
  }

  // 3. A pure standard-library or pure node built-in call (lib.*.d.ts or a
  //    non-effect @types/node module): trusted pure, no edge.
  if (declFile && (LIB_RE.test(declFile) || NODE_TYPES_RE.test(declFile))) {
    resolved++;
    return;
  }

  // 4. A third-party package: Unknown until a `[trust]` line vouches for it. Draw
  //    an edge to a `<package>::<member>` symbol with NO effect root, so
  //    hinzu-core's Unknown handling flags it.
  const pkg = packageFromCall(n, declFile, importMap);
  if (pkg) {
    if (pkg.effect) {
      addEffectLeaf(enclosing, npmSymbol(pkg.name, member), pkg.effect, rel, line);
    } else {
      addEdge(enclosing, npmSymbol(pkg.name, member), "call", rel, line);
      unknownEdges++;
    }
    return;
  }
  // 5. Truly unresolved (any-typed / dynamic dispatch): left out honestly rather
  //    than invented.
}

// The npm package a call goes into: from the callee's declaration file under
// node_modules, else from the import specifier of the call's root identifier.
function packageFromCall(n, declFile, importMap) {
  if (declFile) {
    const m = declFile.match(/\/node_modules\/((?:@[^/]+\/)?[^/]+)\//);
    if (m && !/@types\/node\//.test(declFile) && !LIB_RE.test(declFile)) {
      return { name: m[1].replace(/^@types\//, ""), effect: EFFECTFUL_NPM[m[1]] ?? null };
    }
  }
  const expr = n.expression || (ts.isTaggedTemplateExpression(n) ? n.tag : undefined);
  const id = expr && rootIdentifier(expr);
  const im = id && importMap.get(id.text);
  return im ? { name: im.pkg, effect: im.effect } : null;
}

console.error(
  `hinzu-ts: call sites ${callSites} | resolved ${resolved} | edges ${edges.length} ` +
    `(reference ${refEdges}, unknown ${unknownEdges}) | effect roots ${rootSet.size}`,
);

// --- emit the FactSet JSON ---------------------------------------------------
const out = {
  definitions: [...defs.values()],
  edges,
  effect_roots: [...rootSet.entries()].map(([symbol, effect]) => ({ symbol, effect })),
};
process.stdout.write(JSON.stringify(out, null, 2) + "\n");
