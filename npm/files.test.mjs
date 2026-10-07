// Run with `node --test npm/**/*.test.mjs`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

// npm publishes only what `files` lists. A launcher module missing from it is
// absent from the package, and the installed command fails on its first line.
test("every module the launcher requires is published", () => {
  const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8"));
  const seen = new Set();
  const queue = [pkg.bin.sessionwiki];
  while (queue.length) {
    const file = queue.pop();
    if (seen.has(file)) continue;
    seen.add(file);
    assert.ok(pkg.files.includes(file), `${file} is required by the launcher but not in "files"`);
    const source = readFileSync(new URL(`./${file}`, import.meta.url), "utf8");
    for (const [, rel] of source.matchAll(/require\("\.\/([^"]+)"\)/g)) {
      queue.push(file.replace(/[^/]+$/, rel));
    }
  }
  assert.ok(seen.has("bin/run.js") && seen.has("bin/exit.js"));
});
