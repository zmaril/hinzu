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
// Usage: node analyze.mjs <project-dir> [--tsconfig <path>] [--api]
//
// With `--api`, the adapter runs in PUBLIC-API mode instead of fact mode: it
// resolves the package's real exported interface (package.json `exports`,
// mapping dist→src, following re-exports) via the TypeChecker and writes hinzu's
// language-agnostic API report JSON (the same schema the Rust rustdoc path
// emits) to stdout. See `emitApiReport` at the bottom of this file.

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
let apiMode = false;
for (let i = 0; i < argv.length; i++) {
  if (argv[i] === "--tsconfig") tsconfigArg = argv[++i];
  else if (argv[i] === "--api") apiMode = true;
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
// In API mode the program is rooted at the package's entry points (so it pulls
// in just the public surface's reachable files, not the whole monorepo); in
// fact mode it is rooted at the tsconfig's own file set, exactly as before.
const apiEntryFiles = apiMode ? resolveEntryFiles(ROOT) : null;
const rootNames = apiEntryFiles ?? parsed.fileNames;
const program = ts.createProgram({ rootNames, options: parsed.options });
const checker = program.getTypeChecker();
console.error(
  `hinzu-ts: TypeScript ${ts.version} | root files ${rootNames.length} | ` +
    `program sources ${program.getSourceFiles().length}`,
);

// API mode short-circuits here: emit the public-interface report and exit
// before any fact-extraction code runs.
if (apiMode) {
  emitApiReport(ROOT, program, checker, apiEntryFiles);
  process.exit(0);
}

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

// A type declaration we register as a definition so signature-type edges have a
// local port target: classes, interfaces, type aliases, and enums. Registering
// these (not just function-like nodes) is what lets a `type` edge resolve to a
// local definition — a real port dependency — rather than an external leaf.
function isTypeDeclLike(n) {
  return (
    ts.isClassDeclaration(n) ||
    ts.isClassExpression(n) ||
    ts.isInterfaceDeclaration(n) ||
    ts.isTypeAliasDeclaration(n) ||
    ts.isEnumDeclaration(n)
  );
}

// The display name of a type declaration node (mirrors `nameForNode` for the
// callable case): the declared name, or the variable it is assigned to for an
// anonymous class expression.
function typeNameForNode(n) {
  if (n.name) return n.name.getText();
  const p = n.parent;
  if (p && ts.isVariableDeclaration(p) && p.name) return p.name.getText();
  return "(anonymous type)";
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
    const fnLike = isFunctionLike(n);
    if (fnLike || isTypeDeclLike(n)) {
      const name = fnLike ? nameForNode(n) : typeNameForNode(n);
      const qualified = [...qualifierChain(n), name].filter(Boolean).join(".");
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

// Synthetic per-file `<module>` nodes: the id → {file, line_end} for every owned
// file, and the set of ids that actually earned an edge. A `<module>` definition
// is emitted only for a file whose import-time code attached an edge to it, so
// import-time effects become visible without spawning empty nodes everywhere —
// the same additive discipline as the Python tree-sitter rung.
const moduleMeta = new Map(); // moduleId -> { file, line_end }
const moduleUsed = new Set(); // moduleIds an edge attached to
function moduleIdFor(rel) {
  return `<module>@${rel}`;
}

function addEdge(caller, callee, kind, evFile, evLine) {
  if (!caller || !callee || caller === callee) return;
  if (moduleMeta.has(caller)) moduleUsed.add(caller);
  edges.push({
    caller,
    callee,
    kind,
    resolution: kind,
    evidence_file: evFile,
    evidence_line: evLine,
  });
}
function addEffectLeaf(caller, symbol, effect, evFile, evLine, kind = "call") {
  addEdge(caller, symbol, kind, evFile, evLine);
  rootSet.set(symbol, effect);
}
// A reference-kind effect leaf: a value-position use of an effectful symbol (an
// import-time call, or a higher-order pass-by-value) that call hierarchy's
// call-only view never anchored.
function addRefLeaf(caller, symbol, effect, evFile, evLine) {
  addEffectLeaf(caller, symbol, effect, evFile, evLine, "reference");
}

// --- signature-type dependency edges -----------------------------------------
// A `type` edge means: this function (or class) depends on that type because it
// names it in its parameter types, return type, or extends/implements bounds. It
// is a *porting* dependency, not a call — a function taking a `File` parameter
// does not itself perform any filesystem effect — so it carries no effect root
// and hinzu-core excludes it from effect propagation. It is emitted with
// resolution "reference" (a static resolution to the type's declaration), NOT
// with `addEdge` (whose resolution mirrors the kind and would be an invalid
// "type" resolution). Deduped per (from, to): a type is a structural dependency,
// not a count of mention sites.
let typeEdges = 0;
const typeEdgeSeen = new Set();
function addTypeEdge(caller, callee, evFile, evLine) {
  if (!caller || !callee || caller === callee) return;
  const key = `${caller} ${callee}`;
  if (typeEdgeSeen.has(key)) return;
  typeEdgeSeen.add(key);
  if (moduleMeta.has(caller)) moduleUsed.add(caller);
  edges.push({
    caller,
    callee,
    kind: "type",
    resolution: "reference",
    evidence_file: evFile,
    evidence_line: evLine,
  });
  typeEdges++;
}

// Collect the named type entities inside a type node: every `TypeReferenceNode`'s
// name, walking through generic type arguments, unions, intersections, arrays,
// tuples, parenthesized/optional/rest types, and `extends`-style
// `ExpressionWithTypeArguments`. Skips the structural type operators themselves —
// only *named* references (which can resolve to a declaration) are collected.
function collectTypeNames(typeNode, out) {
  if (!typeNode) return;
  const visit = (t) => {
    if (!t) return;
    if (ts.isTypeReferenceNode(t)) {
      out.push(t.typeName);
      if (t.typeArguments) t.typeArguments.forEach(visit);
      return;
    }
    if (ts.isExpressionWithTypeArguments(t)) {
      out.push(t.expression);
      if (t.typeArguments) t.typeArguments.forEach(visit);
      return;
    }
    ts.forEachChild(t, visit);
  };
  visit(typeNode);
}

// Resolve a type-name entity (an `Identifier` or `QualifiedName`, or a heritage
// expression) to the id of a LOCAL definition, or null. Follows aliases, then
// looks for a declaration that is both owned (in the analyzed project) and
// registered as a definition — a class/interface/type-alias/enum (or, for a
// value used as a base, a function-like). Built-in/lib types resolve to
// `lib.*.d.ts` (not owned) → null → skipped. Type parameters resolve to a
// `TypeParameterDeclaration` (never registered) → null → skipped.
function resolveTypeDefId(nameNode) {
  let s = checker.getSymbolAtLocation(nameNode);
  if (s && s.flags & ts.SymbolFlags.Alias) {
    try {
      s = checker.getAliasedSymbol(s);
    } catch {}
  }
  for (const d of s?.getDeclarations?.() || []) {
    if (!ownedRel(d.getSourceFile().fileName)) continue;
    const id = defIdByNode.get(d);
    if (id) return id;
  }
  return null;
}

// Emit a `type` edge from `fromId` to each local type named in `nameNodes`.
function emitTypeEdgesTo(fromId, nameNodes, rel, sf) {
  if (!fromId) return;
  for (const nameNode of nameNodes) {
    const toId = resolveTypeDefId(nameNode);
    if (toId) addTypeEdge(fromId, toId, rel, lineOf(sf, nameNode.getStart()));
  }
}

// A function/method/arrow depends on the types in its parameter and return
// signature.
function emitSignatureTypeEdges(fnNode, fnId, rel, sf) {
  if (!fnId) return;
  const names = [];
  for (const p of fnNode.parameters || []) collectTypeNames(p.type, names);
  collectTypeNames(fnNode.type, names);
  emitTypeEdgesTo(fnId, names, rel, sf);
}

// A class depends on the types in its extends/implements heritage clauses.
function emitHeritageTypeEdges(classNode, classId, rel, sf) {
  if (!classId) return;
  const names = [];
  for (const clause of classNode.heritageClauses || []) {
    for (const t of clause.types || []) collectTypeNames(t, names);
  }
  emitTypeEdgesTo(classId, names, rel, sf);
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
  if (ts.isTaggedTemplateExpression(p) && p.tag === id) return false; // the call callee, not a value
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

// A property-access node used as a VALUE (`register(fs.readFile)`), not as the
// callee of a call/new/tagged-template applied to it (`fs.readFile(x)`) and not
// an inner link of a longer member chain (`fs.promises` inside `fs.promises.x`,
// where only the outermost access resolves). This is the property-access twin of
// `isReferenceUse`, for higher-order references to effectful members.
function isPropertyRefUse(pa) {
  const p = pa.parent;
  if (!p) return false;
  if ((ts.isCallExpression(p) || ts.isNewExpression(p)) && p.expression === pa) return false;
  if (ts.isTaggedTemplateExpression(p) && p.tag === pa) return false;
  if (ts.isPropertyAccessExpression(p) && p.expression === pa) return false; // inner chain link
  if (ts.isElementAccessExpression(p) && p.expression === pa) return false;
  return true;
}

// A bare identifier used as a value that names an effectful symbol: a node
// built-in named import (`import { readFile } from "node:fs"`) resolved by
// declaration provenance, an ambient effectful global (`fetch`, `WebSocket`)
// confirmed against lib/@types, or a symbol imported from a known-effectful npm
// package. `sym` is the identifier's already-resolved (alias-followed) symbol.
// Returns { symbol, effect } or null. Mirrors the resolution the call path uses,
// for a pass-by-value reference rather than a call.
function classifyIdentifierValue(id, sym, importMap) {
  const declFile = declFilesOfSymbol(sym)[0];
  const nb = nodeBuiltinEffect(declFile, id.text);
  if (nb) return nb;
  const bare = BARE_CALL_EFFECTS[id.text];
  if (bare && symFromLibOrNode(id)) return { symbol: globalSymbol(id.text), effect: bare };
  const im = importMap.get(id.text);
  if (im && im.effect) return { symbol: npmSymbol(im.pkg, id.text), effect: im.effect };
  return null;
}

// A property-access value (`fs.readFile`, `crypto.randomBytes`) whose member
// resolves, by declaration provenance, to an effectful node built-in. Returns
// { symbol, effect } or null. The ambient-global members (`process.env`,
// `Math.random`, …) are handled by `classifyGlobalAccess`, so this is the
// node-built-in twin, reusing the same `nodeBuiltinEffect` provenance the call
// path uses.
function classifyNodeBuiltinValue(node) {
  if (!ts.isPropertyAccessExpression(node)) return null;
  let sym = checker.getSymbolAtLocation(node);
  if (sym && sym.flags & ts.SymbolFlags.Alias) {
    try {
      sym = checker.getAliasedSymbol(sym);
    } catch {}
  }
  const declFile = declFilesOfSymbol(sym)[0];
  return nodeBuiltinEffect(declFile, node.name.text);
}

let callSites = 0;
let resolved = 0;
let refEdges = 0;
let unknownEdges = 0;

for (const sf of program.getSourceFiles()) {
  const rel = ownedRel(sf.fileName);
  if (!rel) continue;
  const importMap = buildImportMap(sf);
  // The file's synthetic `<module>` node: caller for anything at module scope
  // (import-time code call hierarchy never anchors). Registered for every owned
  // file; a definition is emitted later only if an edge actually attaches to it.
  const moduleId = moduleIdFor(rel);
  moduleMeta.set(moduleId, { file: rel, line_end: lineOf(sf, sf.getEnd()) });
  const stack = [];
  const walk = (n) => {
    const isFn = isFunctionLike(n);
    if (isFn) stack.push(defIdByNode.get(n));
    // The caller: the nearest enclosing owned function, or — at module scope —
    // the file's `<module>` node, so import-time effects are attributed rather
    // than dropped. `atModule` picks the edge kind: inside a function, calls are
    // `call` edges (call hierarchy's job, unchanged); at module scope they are
    // `reference` edges, exactly like the Python rung's module-scope call callees.
    const enclosing = stack.length ? stack[stack.length - 1] : null;
    const caller = enclosing ?? moduleId;
    const atModule = enclosing === null;
    const line = lineOf(sf, n.getStart());

    if (caller) {
      // Ambient global effect reached by property access (process.env, Date.now),
      // whether called or used as a value.
      const ga = classifyGlobalAccess(n);
      if (ga) addEffectLeaf(caller, ga.symbol, ga.effect, rel, line, atModule ? "reference" : "call");

      if (ts.isCallExpression(n) || ts.isNewExpression(n) || ts.isTaggedTemplateExpression(n)) {
        callSites++;
        handleCall(n, caller, rel, line, importMap, atModule ? "reference" : "call");
      }

      handleReference(n, caller, rel, line, importMap);
    }

    // Signature-type dependency edges. The caller is the declaration ITSELF (its
    // own def id), not the enclosing function: a function depends on the types in
    // its signature, and a class on its bases. Unlike calls/references, these do
    // not go through `caller`, so they are emitted here regardless of scope.
    if (isFn) {
      emitSignatureTypeEdges(n, defIdByNode.get(n), rel, sf);
    }
    if (ts.isClassDeclaration(n) || ts.isClassExpression(n)) {
      emitHeritageTypeEdges(n, defIdByNode.get(n), rel, sf);
    }

    ts.forEachChild(n, walk);
    if (isFn) stack.pop();
  };
  ts.forEachChild(sf, walk);
}

// Reference-level taint: a value-position use (a bare identifier or an `a.b`
// member) that is NOT the callee of a call — a function or effectful symbol
// passed as a value (callback, default parameter, stored, returned, in an
// array/object literal). Resolved through the SAME declaration → provenance →
// effect path the call resolver uses, so it is sound-additive: it only adds the
// higher-order and module-level effects the call-only view missed. The call
// callee itself is excluded by `isReferenceUse` / `isPropertyRefUse` (dedupe by
// position), so nothing is emitted as both a call and a reference.
function handleReference(n, caller, rel, line, importMap) {
  if (ts.isIdentifier(n) && isReferenceUse(n)) {
    // 1. A function we own, used as a value — taints through its own body edges.
    let s = checker.getSymbolAtLocation(n);
    if (s && s.flags & ts.SymbolFlags.Alias) {
      try {
        s = checker.getAliasedSymbol(s);
      } catch {}
    }
    const fnDecl = (s?.getDeclarations?.() || []).find(isFunctionLike);
    const calleeId = fnDecl && defIdByNode.get(fnDecl);
    if (calleeId && calleeId !== caller) {
      addEdge(caller, calleeId, "reference", rel, line);
      refEdges++;
      return;
    }
    // 2. A node built-in, effectful ambient global, or effectful npm import
    //    passed as a value (`register(fetch)`, `register(readFile)`) — an effect
    //    root reached by reference.
    const ext = classifyIdentifierValue(n, s, importMap);
    if (ext) {
      addRefLeaf(caller, ext.symbol, ext.effect, rel, line);
      refEdges++;
    }
    return;
  }
  // 3. An effectful node built-in member passed as a value (`register(fs.readFile)`).
  //    Ambient-global members (`process.env`, `Math.random`) are already handled
  //    above by `classifyGlobalAccess`; this covers the node-built-in members.
  if (ts.isPropertyAccessExpression(n) && isPropertyRefUse(n)) {
    const ext = classifyNodeBuiltinValue(n);
    if (ext) {
      addRefLeaf(caller, ext.symbol, ext.effect, rel, line);
      refEdges++;
    }
  }
}

// `kind` is the edge kind to emit — "call" for a call inside a function (call
// hierarchy's domain), "reference" for a module-scope call attributed to the
// file's `<module>` node (call hierarchy never anchors import-time code, so the
// reference rung picks it up, matching the Python model).
function handleCall(n, enclosing, rel, line, importMap, kind = "call") {
  const { declNode, declFile } = resolveCallee(n);

  // 1. A call into a function we own: its effects propagate through its own
  //    body's edges.
  const localId = declNode && defIdByNode.get(declNode);
  if (localId) {
    resolved++;
    addEdge(enclosing, localId, kind, rel, line);
    return;
  }

  const member = calleeMember(n);

  // 2. An effectful node built-in or ambient global — an effect root.
  const builtin =
    nodeBuiltinEffect(declFile, member) ||
    (ts.isCallExpression(n) ? classifyBareCall(n) : null);
  if (builtin) {
    resolved++;
    addEffectLeaf(enclosing, builtin.symbol, builtin.effect, rel, line, kind);
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
      addEffectLeaf(enclosing, npmSymbol(pkg.name, member), pkg.effect, rel, line, kind);
    } else {
      addEdge(enclosing, npmSymbol(pkg.name, member), kind, rel, line);
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

// Emit a synthetic `<module>` definition for every file whose import-time code
// actually attached an edge to its `<module>` node — whole-file span, so the
// import-time effect is visible and policeable. Files with no module-scope effect
// spawn no node.
for (const id of moduleUsed) {
  const meta = moduleMeta.get(id);
  if (!meta) continue;
  defs.set(id, {
    id,
    display: "<module>",
    language: "typescript",
    file: meta.file,
    line_start: 1,
    line_end: meta.line_end,
  });
}

console.error(
  `hinzu-ts: call sites ${callSites} | resolved ${resolved} | edges ${edges.length} ` +
    `(reference ${refEdges}, type ${typeEdges}, unknown ${unknownEdges}) | ` +
    `effect roots ${rootSet.size} | module defs ${moduleUsed.size}`,
);

// --- emit the FactSet JSON ---------------------------------------------------
const out = {
  definitions: [...defs.values()],
  edges,
  effect_roots: [...rootSet.entries()].map(([symbol, effect]) => ({ symbol, effect })),
};
process.stdout.write(JSON.stringify(out, null, 2) + "\n");

// ===========================================================================
// PUBLIC-API MODE (`--api`)
//
// Emit hinzu's language-agnostic API report for a package's real public
// interface. The public surface is defined honestly: the symbols re-exported
// from the package's entry points (package.json `exports`, dist→src mapped),
// followed through re-exports via the TypeChecker. An internal `export` that no
// entry point re-exports is NOT public surface and is excluded (its count is
// reported in the fidelity block). Types are rendered strings from the checker
// (`typeToString`); the shape matches the Rust rustdoc path so both normalize
// through the same core `build_api`.
// ===========================================================================

// Map an `exports` target (`./dist/providers/foo.d.ts`, `./dist/index.js`) to
// the source `.ts` file it is built from, keeping any `*` wildcard in place.
function distTargetToSrc(root, target) {
  let t = String(target).replace(/^\.\//, "");
  t = t.replace(/^dist\//, "src/");
  t = t.replace(/\.d\.ts$/, ".ts").replace(/\.[cm]?js$/, ".ts");
  return path.join(root, t);
}

// Every real `.ts` source file under `dir` (recursively), excluding `.d.ts`.
function walkTsFiles(dir) {
  const out = [];
  const visit = (d) => {
    let entries;
    try {
      entries = fs.readdirSync(d, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      const full = path.join(d, e.name);
      if (e.isDirectory()) visit(full);
      else if (e.name.endsWith(".ts") && !e.name.endsWith(".d.ts")) out.push(full);
    }
  };
  visit(dir);
  return out;
}

// Resolve a package's entry-point source files from its package.json `exports`
// (falling back to `main`, then `src/index.ts`). Wildcard subpaths (`./api/*`)
// expand to every matching source file. Returns absolute, existing `.ts` paths.
function resolveEntryFiles(root) {
  const files = new Set();
  const add = (p) => {
    if (fs.existsSync(p)) files.add(p);
  };
  let pkg = {};
  try {
    pkg = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8"));
  } catch {
    /* no package.json — fall through to the src/index.ts default */
  }
  const targetOf = (val) =>
    typeof val === "string" ? val : val && (val.types || val.import || val.default || val.require);
  const consume = (target) => {
    if (!target) return;
    const src = distTargetToSrc(root, target);
    if (!src.includes("*")) {
      add(src);
      return;
    }
    const [pre, post] = src.split("*");
    for (const f of walkTsFiles(path.join(root, "src"))) {
      if (f.startsWith(pre) && f.endsWith(post)) files.add(f);
    }
  };
  const exp = pkg.exports;
  if (typeof exp === "string") consume(exp);
  else if (exp && typeof exp === "object") for (const val of Object.values(exp)) consume(targetOf(val));
  if (files.size === 0) {
    if (pkg.main) consume(pkg.main);
    add(path.join(root, "src/index.ts"));
  }
  return [...files];
}

// A project-relative, forward-slash path for a file under `root`, or null when
// the file lives outside the package (an external re-export).
function srcRel(root, fileName) {
  const rel = path.relative(root, fileName);
  if (!rel || rel.startsWith("..") || path.isAbsolute(rel)) return null;
  return rel.split(path.sep).join("/");
}

function stripTsExt(rel) {
  return rel.replace(/\.d\.ts$/, "").replace(/\.[cm]?tsx?$/, "");
}

// The npm package a declaration file belongs to (`.../node_modules/@scope/name/…`
// → `@scope/name`), or the basename directory as a last resort.
function externalPackageOf(fileName) {
  const m = fileName.match(/node_modules\/((?:@[^/]+\/)?[^/]+)\//);
  return m ? m[1] : "external";
}

function jsDocOf(sym) {
  if (!sym || !sym.getDocumentationComment) return null;
  const s = ts.displayPartsToString(sym.getDocumentationComment(checker)).trim();
  return s.length ? s : null;
}

function isDeprecated(decl) {
  try {
    return ts.getJSDocTags(decl).some((t) => t.tagName.text === "deprecated");
  } catch {
    return false;
  }
}

// Rendered generic parameters of a declaration (`T`, `T extends Foo`).
function genericsOf(decl) {
  const tps = decl && decl.typeParameters;
  if (!tps) return [];
  return tps.map((tp) => {
    const name = tp.name.getText();
    return tp.constraint ? `${name} extends ${tp.constraint.getText()}` : name;
  });
}

function resolveAlias(sym) {
  if (sym && sym.flags & ts.SymbolFlags.Alias) {
    try {
      return checker.getAliasedSymbol(sym);
    } catch {
      /* keep the alias if it cannot be resolved */
    }
  }
  return sym;
}

// The `@throws` type/description on a declaration, rendered — TypeScript models
// no checked exceptions, so this is the only honest error-type source.
function throwsOf(decl) {
  try {
    for (const tag of ts.getJSDocTags(decl)) {
      if (tag.tagName.text !== "throws") continue;
      if (tag.typeExpression && tag.typeExpression.type) return tag.typeExpression.type.getText();
      if (typeof tag.comment === "string" && tag.comment.trim()) return tag.comment.trim();
    }
  } catch {
    /* no JSDoc */
  }
  return null;
}

function paramOf(p, fallbackDecl) {
  const pd = p.valueDeclaration || (p.declarations && p.declarations[0]) || fallbackDecl;
  const ty = checker.typeToString(checker.getTypeOfSymbolAtLocation(p, pd));
  const isParam = pd && ts.isParameter(pd);
  const rest = isParam && !!pd.dotDotDotToken;
  const optional =
    (isParam && (!!pd.questionToken || !!pd.initializer)) || / \| undefined$/.test(ty) || ty === "undefined";
  let dflt = null;
  if (isParam && pd.initializer) {
    const text = pd.initializer.getText();
    if (!text.includes("\n") && text.length <= 40) dflt = text;
  }
  return {
    name: (rest ? "..." : "") + p.getName(),
    ty,
    optional,
    default: dflt,
  };
}

// Build a Signature from a call signature. `receiver` is null for a free
// function, or the owning type name for a method.
function signatureOf(sig, decl, receiver) {
  const params = sig.getParameters().map((p) => paramOf(p, decl));
  const returnType = checker.typeToString(sig.getReturnType());
  const asyncModifier =
    decl && ts.canHaveModifiers(decl)
      ? (ts.getCombinedModifierFlags(decl) & ts.ModifierFlags.Async) !== 0
      : false;
  const isAsync = asyncModifier || /^Promise[<]/.test(returnType) || returnType === "Promise";
  const tps = sig.getTypeParameters();
  const generics = tps ? tps.map((t) => t.symbol.getName()) : genericsOf(decl);
  return {
    params,
    returnType: returnType || null,
    isAsync,
    receiver: receiver || null,
    errorType: throwsOf(decl),
    generics,
  };
}

function fieldOf(prop, fallbackDecl, visibility) {
  const pd = prop.valueDeclaration || (prop.declarations && prop.declarations[0]) || fallbackDecl;
  const ty = pd ? checker.typeToString(checker.getTypeOfSymbolAtLocation(prop, pd)) : "unknown";
  const optional = !!(prop.flags & ts.SymbolFlags.Optional) || / \| undefined$/.test(ty);
  return { name: prop.getName(), ty, visibility, doc: jsDocOf(prop), optional };
}

// The properties of a type as fields (interfaces, object-literal type aliases).
function typeFields(type) {
  if (!type || !type.getProperties) return [];
  return type.getProperties().map((p) => fieldOf(p, null, "public"));
}

// The names in a declaration's heritage clauses (extends / implements).
function heritageNames(decl) {
  const out = [];
  for (const clause of decl.heritageClauses || []) {
    for (const t of clause.types || []) out.push(t.expression.getText());
  }
  return out;
}

// A fresh common item envelope; the caller fills the kind-specific payload.
function baseApiItem(kind, id, name, decl, modulePath, file) {
  const sf = decl.getSourceFile ? decl.getSourceFile() : null;
  const line = sf && file ? sf.getLineAndCharacterOfPosition(decl.getStart()).line + 1 : null;
  return {
    kind,
    id,
    name,
    visibility: "public",
    modulePath,
    file,
    line,
    doc: null,
    generics: [],
    deprecated: isDeprecated(decl),
    signature: null,
    fields: [],
    variants: [],
    implements: [],
    aliasTarget: null,
    constType: null,
    constValue: null,
  };
}

// A public (non-private, non-#) class member declaration?
function isPublicMember(m) {
  if (m.name && ts.isPrivateIdentifier(m.name)) return false;
  const mods = ts.canHaveModifiers(m) ? ts.getCombinedModifierFlags(m) : 0;
  return (mods & (ts.ModifierFlags.Private | ts.ModifierFlags.Protected)) === 0;
}

// Lower one exported symbol into one or more ApiItems (a class yields its own
// item plus a `method` item per public method). Pushes onto `out`.
function lowerExport(exportName, sym, out, seen) {
  const s = resolveAlias(sym);
  // getDeclarations() returns undefined for a declaration-less symbol (e.g. an
  // ambient/synthesized export); guard the index so such symbols are skipped by
  // the `if (!decl)` below rather than throwing.
  const decl = (s.getDeclarations && s.getDeclarations()?.[0]) || null;
  if (!decl) return null;
  const declFile = decl.getSourceFile().fileName;
  const rel = srcRel(ROOT, declFile);
  const inPackage = rel !== null && !rel.endsWith(".d.ts");
  const modulePath = inPackage ? stripTsExt(rel) : `external:${externalPackageOf(declFile)}`;
  const file = inPackage ? rel : null;
  const id = `${modulePath}#${exportName}`;
  if (seen.has(id)) return id;
  seen.add(id);

  const item = baseApiItem(kindOf(s, decl), id, exportName, decl, modulePath, file);
  item.doc = jsDocOf(s);
  item.generics = genericsOf(decl);

  if (item.kind === "function") {
    const t = checker.getTypeOfSymbolAtLocation(s, decl);
    const sig = t.getCallSignatures()[0];
    const sigDecl = ts.isFunctionLike(decl) ? decl : sig && sig.declaration ? sig.declaration : decl;
    if (sig) item.signature = signatureOf(sig, sigDecl, null);
  } else if (item.kind === "class") {
    item.implements = heritageNames(decl);
    for (const m of decl.members || []) {
      if (!isPublicMember(m)) continue;
      if ((ts.isPropertyDeclaration(m) || ts.isGetAccessor(m)) && m.name) {
        const psym = checker.getSymbolAtLocation(m.name);
        if (psym) item.fields.push(fieldOf(psym, m, "public"));
      } else if (ts.isMethodDeclaration(m) && m.name) {
        const msym = checker.getSymbolAtLocation(m.name);
        const mt = msym && checker.getTypeOfSymbolAtLocation(msym, m);
        const sig = mt && mt.getCallSignatures()[0];
        if (!sig) continue;
        const mid = `${id}.${m.name.getText()}`;
        const mi = baseApiItem("method", mid, m.name.getText(), m, modulePath, file);
        mi.doc = jsDocOf(msym);
        mi.signature = signatureOf(sig, m, exportName);
        mi.generics = mi.signature.generics;
        out.push(mi);
      }
    }
  } else if (item.kind === "interface") {
    item.implements = heritageNames(decl);
    item.fields = typeFields(checker.getDeclaredTypeOfSymbol(s));
  } else if (item.kind === "typeAlias") {
    item.aliasTarget = decl.type ? decl.type.getText() : checker.typeToString(checker.getDeclaredTypeOfSymbol(s));
    if (decl.type && ts.isTypeLiteralNode(decl.type)) item.fields = typeFields(checker.getDeclaredTypeOfSymbol(s));
  } else if (item.kind === "enum") {
    for (const m of decl.members || []) {
      const val = m.initializer ? m.initializer.getText() : String(checker.getConstantValue(m) ?? "");
      item.variants.push({
        name: m.name.getText(),
        fields: [],
        discriminant: val.length ? val : null,
        doc: jsDocOf(checker.getSymbolAtLocation(m.name)),
      });
    }
  } else if (item.kind === "const") {
    item.constType = checker.typeToString(checker.getTypeOfSymbolAtLocation(s, decl));
    if (ts.isVariableDeclaration(decl) && decl.initializer) {
      const text = decl.initializer.getText();
      if (!text.includes("\n") && text.length <= 60) item.constValue = text;
    }
  }
  out.push(item);
  return id;
}

// The item kind for an exported symbol.
function kindOf(s, decl) {
  const F = ts.SymbolFlags;
  if (s.flags & F.Function) return "function";
  if (s.flags & F.Class) return "class";
  if (s.flags & F.Interface) return "interface";
  if (s.flags & F.Enum) return "enum";
  if (s.flags & F.TypeAlias) return "typeAlias";
  if (s.flags & (F.Module | F.NamespaceModule)) return "namespace";
  if (s.flags & (F.Variable | F.BlockScopedVariable | F.FunctionScopedVariable)) {
    const t = checker.getTypeOfSymbolAtLocation(s, decl);
    if (t.getCallSignatures().length > 0) return "function";
    return "const";
  }
  return "const";
}

// Count in-package exports that no entry point re-exports (the internal-only
// surface excluded from the report), for the fidelity block.
function countInternalOnly(root, program, publicIds) {
  const allIds = new Set();
  for (const sf of program.getSourceFiles()) {
    const rel = srcRel(root, sf.fileName);
    if (!rel || rel.endsWith(".d.ts") || !rel.startsWith("src/")) continue;
    const msym = checker.getSymbolAtLocation(sf);
    if (!msym) continue;
    for (const ex of checker.getExportsOfModule(msym)) {
      const s = resolveAlias(ex);
      const d = s.getDeclarations && s.getDeclarations()?.[0];
      if (!d) continue;
      const dr = srcRel(root, d.getSourceFile().fileName);
      if (!dr || dr.endsWith(".d.ts")) continue;
      allIds.add(`${stripTsExt(dr)}#${ex.getName()}`);
    }
  }
  let n = 0;
  for (const id of allIds) if (!publicIds.has(id)) n++;
  return n;
}

// Drive the whole API extraction and write the report JSON to stdout.
function emitApiReport(root, program, checker, entryFiles) {
  const seen = new Set();
  const items = [];
  let entryCount = 0;
  for (const entry of entryFiles) {
    const sf = program.getSourceFile(entry);
    if (!sf) {
      console.error(`hinzu-ts: api: entry not in program: ${entry}`);
      continue;
    }
    const msym = checker.getSymbolAtLocation(sf);
    if (!msym) continue;
    entryCount++;
    for (const ex of checker.getExportsOfModule(msym)) lowerExport(ex.getName(), ex, items, seen);
  }

  // Group by module (declaring file / external package).
  const modules = new Map();
  for (const it of items) {
    let m = modules.get(it.modulePath);
    if (!m) {
      m = { path: it.modulePath, file: it.file, doc: null, items: [] };
      modules.set(it.modulePath, m);
    }
    m.items.push(it);
  }
  // Per-module doc from the source file's own module JSDoc, where cheap.
  for (const m of modules.values()) {
    if (!m.file) continue;
    const sf = program.getSourceFile(path.join(root, m.file));
    const msym = sf && checker.getSymbolAtLocation(sf);
    if (msym) m.doc = jsDocOf(msym);
  }

  const publicInPackage = new Set(items.filter((i) => i.file).map((i) => i.id));
  const internalOnly = countInternalOnly(root, program, publicInPackage);

  let pkg = {};
  try {
    pkg = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8"));
  } catch {
    /* default package fields below */
  }
  const report = {
    package: {
      name: pkg.name || path.basename(root),
      language: "typescript",
      root: path.relative(process.cwd(), root) || ".",
      version: pkg.version || null,
    },
    fidelity: {
      source: "tsc",
      format_version: ts.version,
      complete: false,
      notes: [
        `Public surface = symbols re-exported from ${entryCount} entry source file(s) resolved ` +
          "from package.json exports (dist→src mapped, wildcard subpaths expanded, re-exports " +
          `followed); an export never reachable from an entry point is excluded (${internalOnly} ` +
          "internal-only export(s) omitted).",
        "Types are rendered strings from the TypeChecker (typeToString), not cross-referenced ids.",
        "TypeScript has structural union types: a `type X = 'a' | 'b'` is a typeAlias, not an enum; " +
          "only real `enum` declarations are emitted as kind=enum.",
        "Overloaded functions: only the first call signature is emitted.",
        "errorType is populated only from a JSDoc @throws tag; TypeScript models no checked exceptions.",
        "Interface members (including methods) are reported as fields with a rendered type; class " +
          "methods are separate `method` items and static members are omitted.",
        "Symbols re-exported from a dependency are grouped under an `external:<pkg>` module with a null file.",
      ],
    },
    modules: [...modules.values()],
  };
  // hinzu_api_version is stamped by core's build_api after the CLI passes
  // package/fidelity/modules through it for normalization + sorting; this echo
  // is only for a human eyeballing the adapter's raw stdout.
  report.hinzu_api_version = 1;

  console.error(
    `hinzu-ts: api: ${items.length} public items across ${modules.size} modules ` +
      `(${internalOnly} internal-only excluded)`,
  );
  // Write synchronously to fd 1: the caller then `process.exit(0)`s, and a piped
  // stdout is asynchronous, so `process.stdout.write` could be truncated on exit.
  fs.writeSync(1, JSON.stringify(report, null, 2) + "\n");
}
