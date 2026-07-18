// The adapter layer: this file is allowed to touch the filesystem.
import { readFileSync } from "node:fs";

export function readConfig(pathToConfig: string): string {
  // A real filesystem effect — the leaf the analysis seeds as an `fs` root.
  return readFileSync(pathToConfig, "utf8");
}
