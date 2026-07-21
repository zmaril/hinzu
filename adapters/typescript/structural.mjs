// The hinzu TypeScript *structural* extractor behind `hinzu similar`: build one
// Program from the project's tsconfig (or a source glob), walk every
// function-like declaration in an OWNED file, and reduce each body to a
// language-neutral `StructuralSignature` the pure similarity engine
// (`hinzu_core::similarity`) consumes. It is a sibling of `analyze.mjs` (the
// effect-fact extractor) and reuses its Program/ownership patterns.
//
// The key advantage over the Rust/syn extractor is that this one uses the tsc
// CHECKER to RESOLVE parameter/return types before erasing identifiers to `_`,
// so `Promise<User>` and `Promise<Order>` produce the same type shape
// `Promise<_>` and match. That honesty (and its limits — `any`/`unknown` and
// structural typing erode it) is carried by the TypeScript language profile
// shipped in the core, so every finding states what this extraction could and
// could not see.
//
// Output (stdout) is exactly the shape `hinzu_core::similarity::SignatureDoc`
// deserializes:
//   { language: "typescript", extractor: "tsc-checker", signatures: [ ... ] }
// All diagnostics go to stderr so stdout stays pure JSON.
//
// Usage: node structural.mjs <project-dir> [--tsconfig <path>]

import ts from "typescript";
import path from "node:path";
import fs from "node:fs";
import {
  parseArgs,
  programFromTsconfig,
  makeOwnedRel,
  forEachOwnedSourceFile,
  isFunctionLike,
  nameForNode,
  qualifierChain,
  lineOf,
} from "./common.mjs";

// The k in the k-gram shingles, fixed to match the engine + the Rust extractor
// (`hinzu_core::similarity::SHINGLE_K` / structural_rust.rs).
const SHINGLE_K = 3;

// --- argument parsing --------------------------------------------------------
const { projectArg, tsconfigArg } = parseArgs(
  process.argv.slice(2),
  "usage: node structural.mjs <project-dir> [--tsconfig <path>]",
);
const ROOT = path.resolve(projectArg);

// --- build the Program -------------------------------------------------------
// Prefer the project's own tsconfig (so the checker resolves the project's own
// types the way its `tsc` would). Fall back to a plain source glob when there is
// no tsconfig, so a loose directory of `.ts` files still analyzes rather than
// failing — the checker then resolves what it can from the bundled lib.d.ts.
const tsconfigPath = tsconfigArg
  ? path.resolve(tsconfigArg)
  : ts.findConfigFile(ROOT, ts.sys.fileExists, "tsconfig.json");

let program;
if (tsconfigPath && fs.existsSync(tsconfigPath)) {
  const built = programFromTsconfig(tsconfigPath);
  program = built.program;
  const parsed = built.parsed;
  console.error(
    `hinzu-ts-structural: TypeScript ${ts.version} via ${path.relative(ROOT, tsconfigPath) || "tsconfig.json"} | ` +
      `root files ${parsed.fileNames.length} | program sources ${program.getSourceFiles().length}`,
  );
} else {
  const rootNames = globTsFiles(ROOT);
  if (rootNames.length === 0) {
    console.error(`no tsconfig.json and no .ts/.tsx source found under ${ROOT}`);
    process.exit(2);
  }
  program = ts.createProgram({
    rootNames,
    options: { allowJs: false, noEmit: true, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
  });
  console.error(
    `hinzu-ts-structural: TypeScript ${ts.version} via source glob (no tsconfig) | ` +
      `root files ${rootNames.length} | program sources ${program.getSourceFiles().length}`,
  );
}
const checker = program.getTypeChecker();

// Every `.ts`/`.tsx` file under a directory, skipping dependency/build dirs and
// declaration files — the source-glob fallback's root set.
function globTsFiles(root) {
  const out = [];
  const walk = (dir) => {
    let entries;
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      const p = path.join(dir, e.name);
      if (e.isDirectory()) {
        if (/^(node_modules|dist|build|out|coverage|\.git)$/.test(e.name)) continue;
        walk(p);
      } else if (/\.[cm]?tsx?$/.test(e.name) && !e.name.endsWith(".d.ts")) {
        out.push(p);
      }
    }
  };
  walk(root);
  return out;
}

// --- which files we own (attribute signatures to) ----------------------------
// The same ownership filter analyze.mjs uses (see common.mjs): real project
// source, not a dependency, not a declaration file, not build output.
const ownedRel = makeOwnedRel(ROOT);

// Naming (`nameForNode`, `qualifierChain`), `lineOf`, and `isFunctionLike` are
// shared with analyze.mjs — see common.mjs.

// The def kind, in the engine's vocabulary. A method/accessor/constructor is a
// "method"; a named function declaration is a "function"; an arrow or function
// expression is a "closure".
function defKind(n) {
  if (
    ts.isMethodDeclaration(n) ||
    ts.isConstructorDeclaration(n) ||
    ts.isGetAccessorDeclaration(n) ||
    ts.isSetAccessorDeclaration(n)
  ) {
    return "method";
  }
  if (ts.isFunctionDeclaration(n)) return "function";
  return "closure"; // arrow / function expression
}

// --- type-shape erasure (the checker does the resolving) ---------------------
// Reduce a RESOLVED `ts.Type` to its structural shape, erasing nominal
// identifiers to `_` while keeping constructors: `Promise<User>` -> `Promise<_>`,
// `Array<string>` -> `Array<_>`, `User | null` -> `_ | _`, `Record<string,string>`
// -> `Record<_,_>`, an anonymous object literal type -> `object`, and a bare
// nominal (`User`, `string`, `number`) -> `_`. Because two functions with the
// same shape but different concrete types produce the same string, this is the
// strong "same shape, different types" signal — RESOLVED, not syntactic.
function typeShape(type, depth = 0) {
  if (!type || depth > 6) return "_";
  try {
    if (type.isUnion && type.isUnion()) {
      const parts = dedupeKeepOrder(type.types.map((t) => typeShape(t, depth + 1)));
      return parts.length === 1 ? parts[0] : parts.join(" | ");
    }
    if (type.isIntersection && type.isIntersection()) {
      return type.types.map((t) => typeShape(t, depth + 1)).join(" & ");
    }
    if (checker.isTupleType && checker.isTupleType(type)) {
      const args = safeTypeArgs(type);
      return "[" + args.map((t) => typeShape(t, depth + 1)).join(",") + "]";
    }
    if (checker.isArrayType && checker.isArrayType(type)) {
      const args = safeTypeArgs(type);
      const inner = args.length ? args.map((t) => typeShape(t, depth + 1)).join(",") : "_";
      return `Array<${inner}>`;
    }
    const objectFlags = type.flags & ts.TypeFlags.Object ? ts.getObjectFlags(type) : 0;
    const sym = type.getSymbol ? type.getSymbol() : undefined;
    // A generic instantiation (a type reference carrying arguments): keep the
    // constructor name, erase the arguments recursively.
    if (objectFlags & ts.ObjectFlags.Reference) {
      const args = safeTypeArgs(type);
      if (args.length > 0 && sym) {
        return `${sym.getName()}<${args.map((t) => typeShape(t, depth + 1)).join(",")}>`;
      }
    }
    if (type.flags & ts.TypeFlags.Object) {
      // A named class/interface/enum with no type arguments is a nominal leaf.
      if (
        sym &&
        sym.flags & (ts.SymbolFlags.Class | ts.SymbolFlags.Interface | ts.SymbolFlags.Enum)
      ) {
        return "_";
      }
      // Anything else object-shaped (an anonymous object literal type, a mapped
      // type, an interface without a resolvable symbol) collapses to `object`.
      return "object";
    }
  } catch {
    return "_";
  }
  // Primitives, literals, type parameters, any/unknown/void/never — all erase to
  // the nominal leaf `_`.
  return "_";
}

function safeTypeArgs(type) {
  try {
    return checker.getTypeArguments(type) || [];
  } catch {
    return [];
  }
}

function dedupeKeepOrder(arr) {
  const seen = new Set();
  const out = [];
  for (const x of arr) {
    if (!seen.has(x)) {
      seen.add(x);
      out.push(x);
    }
  }
  return out;
}

// The erased shape of one parameter: resolve its type through the checker, then
// erase. Falls back to the syntactic type node, then to `_`.
function paramShape(param) {
  try {
    const t = checker.getTypeAtLocation(param);
    return typeShape(t);
  } catch {
    return "_";
  }
}

// The erased result shape and whether it counts as a result (0 for void/undefined).
function returnShape(fnNode) {
  let type;
  try {
    const sig = checker.getSignatureFromDeclaration(fnNode);
    if (sig) type = checker.getReturnTypeOfSignature(sig);
  } catch {
    // fall through
  }
  if (!type && fnNode.type) {
    try {
      type = checker.getTypeFromTypeNode(fnNode.type);
    } catch {
      // fall through
    }
  }
  if (!type) return { shape: "_", isResult: false };
  const isVoid = type.flags & (ts.TypeFlags.Void | ts.TypeFlags.Undefined | ts.TypeFlags.Never);
  return { shape: typeShape(type), isResult: !isVoid };
}

// --- body reduction ----------------------------------------------------------
// A normalized AST-node-kind for a body node, or null for structural noise
// (blocks, tokens, type nodes) that does not enter the fingerprint. Mirrors the
// Rust body visitor's kind vocabulary as closely as the two grammars allow.
function classify(n) {
  switch (n.kind) {
    case ts.SyntaxKind.VariableDeclaration:
      return "let";
    case ts.SyntaxKind.CallExpression:
      return ts.isPropertyAccessExpression(n.expression) ? "method_call" : "call";
    case ts.SyntaxKind.NewExpression:
    case ts.SyntaxKind.TaggedTemplateExpression:
      return "call";
    case ts.SyntaxKind.IfStatement:
    case ts.SyntaxKind.ConditionalExpression:
      return "if";
    case ts.SyntaxKind.SwitchStatement:
      return "match";
    case ts.SyntaxKind.ForStatement:
    case ts.SyntaxKind.ForInStatement:
    case ts.SyntaxKind.ForOfStatement:
    case ts.SyntaxKind.WhileStatement:
    case ts.SyntaxKind.DoStatement:
      return "loop";
    case ts.SyntaxKind.TryStatement:
      return "try";
    case ts.SyntaxKind.CatchClause:
      return "catch";
    case ts.SyntaxKind.AwaitExpression:
      return "await";
    case ts.SyntaxKind.ReturnStatement:
      return "return";
    case ts.SyntaxKind.ThrowStatement:
      return "throw";
    case ts.SyntaxKind.BinaryExpression:
      return isAssignmentOperator(n.operatorToken.kind) ? "assign" : "binary";
    case ts.SyntaxKind.PrefixUnaryExpression:
    case ts.SyntaxKind.PostfixUnaryExpression:
      return "unary";
    case ts.SyntaxKind.PropertyAccessExpression:
      return "field";
    case ts.SyntaxKind.ElementAccessExpression:
      return "index";
    case ts.SyntaxKind.ObjectLiteralExpression:
      return "struct";
    case ts.SyntaxKind.ArrayLiteralExpression:
      return "array";
    case ts.SyntaxKind.SpreadElement:
    case ts.SyntaxKind.SpreadAssignment:
      return "spread";
    case ts.SyntaxKind.ArrowFunction:
    case ts.SyntaxKind.FunctionExpression:
      return "closure";
    case ts.SyntaxKind.Identifier:
      return "path";
    case ts.SyntaxKind.StringLiteral:
    case ts.SyntaxKind.NumericLiteral:
    case ts.SyntaxKind.BigIntLiteral:
    case ts.SyntaxKind.NoSubstitutionTemplateLiteral:
    case ts.SyntaxKind.TemplateExpression:
    case ts.SyntaxKind.TrueKeyword:
    case ts.SyntaxKind.FalseKeyword:
    case ts.SyntaxKind.NullKeyword:
      return "lit";
    default:
      return null;
  }
}

function isAssignmentOperator(kind) {
  return (
    kind >= ts.SyntaxKind.FirstAssignment && kind <= ts.SyntaxKind.LastAssignment
  );
}

// The normalized callee simple-name of a call/new/tagged-template: the final
// identifier, with the receiver and any generic arguments stripped.
function calleeName(n) {
  let e;
  if (ts.isTaggedTemplateExpression(n)) e = n.tag;
  else e = n.expression;
  if (!e) return null;
  if (ts.isIdentifier(e)) return e.text;
  if (ts.isPropertyAccessExpression(e)) return e.name.text;
  if (ts.isParenthesizedExpression(e)) return null;
  return null;
}

// Reduce a function body to its structural counts: the pre-order node-kind
// sequence (for shingles + histogram + token_len), the control-flow skeleton,
// and the ordered call sequence. Type annotation subtrees are skipped so the
// fingerprint is code structure only.
function reduceBody(body) {
  const kinds = [];
  const calls = [];
  const cfg = {
    branch_count: 0,
    match_arms: 0,
    loop_count: 0,
    try_count: 0,
    return_points: 0,
    max_nesting: 0,
  };
  const feat = { has_await: false, has_throw: false, has_try: false };
  let depth = 0;

  const visit = (n) => {
    if (ts.isTypeNode(n)) return; // never descend into type annotations
    const block = ts.isBlock(n) || ts.isCaseBlock(n);
    if (block) {
      depth++;
      if (depth > cfg.max_nesting) cfg.max_nesting = depth;
    }

    const kind = classify(n);
    if (kind) kinds.push(kind);

    switch (n.kind) {
      case ts.SyntaxKind.IfStatement:
      case ts.SyntaxKind.ConditionalExpression:
        cfg.branch_count++;
        break;
      case ts.SyntaxKind.SwitchStatement:
        cfg.match_arms += n.caseBlock.clauses.length;
        break;
      case ts.SyntaxKind.ForStatement:
      case ts.SyntaxKind.ForInStatement:
      case ts.SyntaxKind.ForOfStatement:
      case ts.SyntaxKind.WhileStatement:
      case ts.SyntaxKind.DoStatement:
        cfg.loop_count++;
        break;
      case ts.SyntaxKind.TryStatement:
        cfg.try_count++;
        feat.has_try = true;
        break;
      case ts.SyntaxKind.AwaitExpression:
        cfg.try_count++; // await is TS's suspension/propagation point, like Rust's `?`
        feat.has_await = true;
        break;
      case ts.SyntaxKind.ReturnStatement:
        cfg.return_points++;
        break;
      case ts.SyntaxKind.ThrowStatement:
        feat.has_throw = true;
        break;
      case ts.SyntaxKind.CallExpression:
      case ts.SyntaxKind.NewExpression:
      case ts.SyntaxKind.TaggedTemplateExpression: {
        const c = calleeName(n);
        if (c) calls.push(c);
        break;
      }
      default:
        break;
    }

    ts.forEachChild(n, visit);
    if (block) depth--;
  };
  visit(body);

  return { kinds, calls, cfg, feat };
}

// --- shingles + histogram ----------------------------------------------------
// FNV-1a over a string, computed 64-bit (like the Rust extractor) then reduced
// to a 53-bit value so it serializes as a safe JSON integer the Rust `u64` field
// accepts. Cross-language shingle matching is out of scope for v1, so TS keeps
// its own self-consistent hashing; only within-TS set overlap matters.
const FNV_OFFSET = 0xcbf29ce484222325n;
const FNV_PRIME = 0x100000001b3n;
const U64_MASK = 0xffffffffffffffffn;
const SAFE53_MASK = 0x1fffffffffffffn;
function fnv1a53(s) {
  let hash = FNV_OFFSET;
  for (let i = 0; i < s.length; i++) {
    hash ^= BigInt(s.charCodeAt(i) & 0xff);
    hash = (hash * FNV_PRIME) & U64_MASK;
  }
  return Number(hash & SAFE53_MASK);
}

function shingles(kinds) {
  if (kinds.length === 0) return [];
  if (kinds.length < SHINGLE_K) return [fnv1a53(kinds.join("|"))];
  const out = [];
  for (let i = 0; i + SHINGLE_K <= kinds.length; i++) {
    out.push(fnv1a53(kinds.slice(i, i + SHINGLE_K).join("|")));
  }
  return out;
}

function histogram(kinds) {
  const h = {};
  for (const k of kinds) h[k] = (h[k] || 0) + 1;
  return h;
}

// --- decorators --------------------------------------------------------------
function hasDecorator(n) {
  const mods = ts.canHaveDecorators && ts.canHaveDecorators(n) ? ts.getDecorators(n) : undefined;
  if (mods && mods.length) return true;
  // A method/property whose parent carries decorators is not itself decorated;
  // only the node's own decorators count.
  return false;
}

function isAsync(n) {
  const mods = n.modifiers || [];
  return mods.some((m) => m.kind === ts.SyntaxKind.AsyncKeyword);
}

function isGenerator(n) {
  return !!n.asteriskToken;
}

// --- build one signature -----------------------------------------------------
const signatures = [];
const idSeen = new Map(); // base id -> count, for stable de-duplication

function buildSignature(n, sf, rel, relNoExt) {
  const body = n.body;
  if (!body) return; // overload / ambient / abstract — no body to fingerprint

  const name = nameForNode(n);
  const qualified = [...qualifierChain(n), name].filter(Boolean).join(".");
  let symbolId = `${relNoExt}#${qualified}`;
  const seen = idSeen.get(symbolId) || 0;
  idSeen.set(symbolId, seen + 1);
  if (seen > 0) symbolId += `@${lineOf(sf, n.getStart())}~${seen}`;

  const { kinds, calls, cfg, feat } = reduceBody(body);

  const params = n.parameters || [];
  const ret = returnShape(n);
  const arity = {
    params: params.length,
    results: ret.isResult ? 1 : 0,
    generics: (n.typeParameters && n.typeParameters.length) || 0,
  };
  const type_shape = {
    params: params.map(paramShape),
    result: ret.shape,
  };

  const features = {};
  if (isAsync(n)) features.is_async = "true";
  if (feat.has_await) features.has_await = "true";
  if (feat.has_try) features.has_try = "true";
  if (feat.has_throw) features.has_throw = "true";
  if (isGenerator(n)) features.is_generator = "true";
  if (hasDecorator(n)) features.has_decorator = "true";

  signatures.push({
    symbol_id: symbolId,
    display: name,
    language: "typescript",
    kind: defKind(n),
    file: rel,
    line_start: lineOf(sf, n.getStart()),
    line_end: lineOf(sf, n.getEnd()),
    arity,
    cfg,
    stmt_histogram: histogram(kinds),
    call_sequence: calls,
    type_shape,
    shingles: shingles(kinds),
    token_len: kinds.length,
    features,
  });
}

forEachOwnedSourceFile(program, ownedRel, (sf, rel, relNoExt) => {
  const walk = (n) => {
    if (isFunctionLike(n)) buildSignature(n, sf, rel, relNoExt);
    ts.forEachChild(n, walk);
  };
  ts.forEachChild(sf, walk);
});

console.error(`hinzu-ts-structural: signatures ${signatures.length}`);

// --- emit the SignatureDoc JSON ----------------------------------------------
const out = {
  language: "typescript",
  extractor: "tsc-checker",
  signatures,
};
process.stdout.write(JSON.stringify(out, null, 2) + "\n");
