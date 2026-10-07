// The launcher decides what to report when the prebuilt binary is done. Run
// with `node --test npm/bin/`.
import assert from "node:assert/strict";
import { test } from "node:test";

import { describeExit } from "./exit.js";

test("a normal exit is passed through untouched", () => {
  assert.deepEqual(describeExit({ status: 0, signal: null }), { code: 0, note: null });
  assert.deepEqual(describeExit({ status: 2, signal: null }), { code: 2, note: null });
});

test("a binary killed by a signal is not reported as exit 1", () => {
  // `spawnSync` sets status to null and names the signal when the child is
  // killed; exiting 1 would hide that.
  const killed = describeExit({ status: null, signal: "SIGKILL" });
  assert.equal(killed.code, 137, "128 + 9, the shell convention for a signal");
  assert.match(killed.note, /SIGKILL/);

  const term = describeExit({ status: null, signal: "SIGTERM" });
  assert.equal(term.code, 143);
  assert.match(term.note, /SIGTERM/);
});

test("no status and no signal says so rather than inventing a cause", () => {
  const odd = describeExit({ status: null, signal: null });
  assert.equal(odd.code, 1);
  assert.match(odd.note, /without an exit code/);
});
