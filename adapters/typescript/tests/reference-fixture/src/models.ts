// Module-level network I/O — the flagship of the reference rung.
//
// The bootstrap request is issued at IMPORT time, at module scope, so a
// call-only walk (which only descends function bodies) never saw it: this file
// emitted no edges at all. The reference rung attributes module-scope use to
// this file's synthetic `<module>` node, and the ambient `fetch` surfaces `net`
// — exactly the import-time network effect call-only could not fire.

// A top-level `fetch(...)` runs when this module is imported.
export const BOOTSTRAP = fetch("https://example.com/bootstrap");
