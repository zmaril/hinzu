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
