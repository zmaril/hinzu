// Shared TypeScript-adapter plumbing used by both extractors: `analyze.mjs` (the
// effect-fact extractor) and `structural.mjs` (the structural-signature
// extractor). Both build a Program the same way, own the same set of files, and
// name declaration nodes identically — that shared shape lives here so the two
// entry scripts don't carry verbatim copies of it.

import ts from "typescript";
import path from "node:path";
import fs from "node:fs";

// --- argument parsing --------------------------------------------------------
// The two extractors take the same CLI shape: `<project-dir> [--tsconfig <path>]`.
// `usage` is the script-specific one-line message printed before exit(2).
export function parseArgs(argv, usage) {
  let projectArg = null;
  let tsconfigArg = null;
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--tsconfig") tsconfigArg = argv[++i];
    else if (!projectArg) projectArg = argv[i];
  }
  if (!projectArg) {
    console.error(usage);
    process.exit(2);
  }
  return { projectArg, tsconfigArg };
}

// --- build the Program from a project's own tsconfig -------------------------
// Parse the tsconfig and create the Program the way the project's `tsc` would.
// `rootNamesOverride` (used by analyze.mjs's `--api` mode to root at a package's
// entry points rather than the whole tsconfig file set) replaces the root file
// set when given; otherwise the tsconfig's own `fileNames` are used. Returns
// { program, parsed, rootNames } so the caller can log root/source counts;
// tsconfig diagnostics go to stderr.
export function programFromTsconfig(tsconfigPath, rootNamesOverride) {
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
  const rootNames = rootNamesOverride ?? parsed.fileNames;
  const program = ts.createProgram({ rootNames, options: parsed.options });
  return { program, parsed, rootNames };
}

// --- which files we own (attribute definitions/signatures to) ----------------
// Real project source, not a dependency, not a declaration file, not build
// output. Tests are kept; a policy's `ignore` globs, not the adapter, decide
// whether to skip them.
export const IGNORED_DIRS = /(^|\/)(node_modules|dist|build|out|coverage|\.git)(\/|$)/;

// Build the ownership filter for a given project root: fileName -> unix relpath,
// or null when the file is not owned.
export function makeOwnedRel(root) {
  return function ownedRel(fileName) {
    const rel = path.relative(root, fileName);
    if (!rel || rel.startsWith("..") || path.isAbsolute(rel)) return null;
    const unix = rel.split(path.sep).join("/");
    if (IGNORED_DIRS.test("/" + unix)) return null;
    if (unix.endsWith(".d.ts")) return null;
    return unix;
  };
}

// --- declaration naming ------------------------------------------------------
// True for any function-like declaration node the extractors treat as a callable.
export function isFunctionLike(n) {
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

// The display name of a callable node: the declared name, "constructor" for a
// constructor, or the binding it is assigned to for an anonymous expression.
export function nameForNode(n) {
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

// The enclosing class/namespace qualifier segments, outermost first.
export function qualifierChain(n) {
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

// 1-based line number of a source position.
export function lineOf(sf, pos) {
  return sf.getLineAndCharacterOfPosition(pos).line + 1;
}

// Walk every owned source file in `program`, invoking `visit(sf, rel, relNoExt)`
// for each — `rel` is the unix project-relative path, `relNoExt` the same with
// the `.ts`/`.tsx`/`.mts`/… extension stripped (the def-id prefix). The two
// extractors share this driver and supply their own per-file node walk.
export function forEachOwnedSourceFile(program, ownedRel, visit) {
  for (const sf of program.getSourceFiles()) {
    const rel = ownedRel(sf.fileName);
    if (!rel) continue;
    const relNoExt = rel.replace(/\.[cm]?tsx?$/, "");
    visit(sf, rel, relNoExt);
  }
}
