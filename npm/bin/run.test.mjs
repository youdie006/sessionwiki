// Run with `node --test npm/**/*.test.mjs`.
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const runJs = fileURLToPath(new URL("./run.js", import.meta.url));

const alive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// The launcher is what a process manager or an MCP host signals, but the work
// happens in the binary it started. Killing the launcher left the binary
// running with init as its parent - for `sessionwiki web`, still holding its
// port. The launcher must hand the signal on and wait for the binary.
test(
  "a signal to the launcher reaches the binary",
  { skip: process.platform === "win32" },
  async () => {
    const launcher = spawn(
      process.execPath,
      ["-e", `require(${JSON.stringify(runJs)}).run("sleep", ["30"])`],
      { stdio: "ignore" }
    );
    let child = null;
    for (let i = 0; i < 50 && !child; i++) {
      await sleep(50);
      try {
        child = Number(execFileSync("pgrep", ["-P", String(launcher.pid)]).toString().trim());
      } catch {
        child = null;
      }
    }
    assert.ok(child, "the launcher started the binary");

    const ended = new Promise((resolve) => launcher.on("exit", (code, signal) => resolve({ code, signal })));
    launcher.kill("SIGTERM");
    const { code, signal } = await ended;

    for (let i = 0; i < 40 && alive(child); i++) await sleep(50);
    const orphaned = alive(child);
    if (orphaned) process.kill(child, "SIGKILL");
    assert.equal(orphaned, false, "the binary outlived its launcher");
    assert.equal(signal, null, "the launcher waited and exited with the binary's status");
    assert.equal(code, 143, "128 + SIGTERM, as the binary ended");
  }
);

test("the binary's own exit status passes through", { skip: process.platform === "win32" }, async () => {
  const launcher = spawn(
    process.execPath,
    ["-e", `require(${JSON.stringify(runJs)}).run("sh", ["-c", "exit 3"])`],
    { stdio: "ignore" }
  );
  const code = await new Promise((resolve) => launcher.on("exit", (c) => resolve(c)));
  assert.equal(code, 3);
});
