// A pure-looking functional core for the reference-edge rung.
//
// `scheduleAudit` LOOKS pure — it only pushes onto a list — but it hands
// `readFile` (a filesystem effect from `node:fs`) to `register` as a *value*.
// TypeScript's call resolution is call-only, so it never saw that higher-order
// use; the compiler-API reference rung does, so `scheduleAudit` now reaches `fs`
// and the functional-core policy flags it. `pureTotal` is genuinely pure and
// must stay unflagged.
import { readFile } from "node:fs";

type Hook = (...args: unknown[]) => unknown;

const HOOKS: Hook[] = [];

export function register(hook: Hook): void {
  HOOKS.push(hook);
}

export function scheduleAudit(): void {
  // `readFile` is passed as a VALUE, never called here — a higher-order
  // reference. It reaches the filesystem when a hook runner later invokes it,
  // so the core is not effect-free after all.
  register(readFile);
}

export function pureTotal(xs: number[]): number {
  return xs.reduce((a, b) => a + b, 0);
}
