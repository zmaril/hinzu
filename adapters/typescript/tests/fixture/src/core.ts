// The functional core: no effects are allowed here, however deep the call chain.
// `parse` and `countKeys` are genuinely pure; `loadAndSummarize` is the leak —
// it reaches the filesystem through the adapter, so the policy must flag it.
import { readConfig } from "./io.js";

export function parse(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of text.split("\n")) {
    const eq = line.indexOf("=");
    if (eq > 0) out[line.slice(0, eq).trim()] = line.slice(eq + 1).trim();
  }
  return out;
}

export function countKeys(config: Record<string, string>): number {
  return Object.keys(config).length;
}

// Looks like plain core logic, but transitively performs filesystem I/O.
export function loadAndSummarize(pathToConfig: string): number {
  const config = parse(readConfig(pathToConfig));
  return countKeys(config);
}
