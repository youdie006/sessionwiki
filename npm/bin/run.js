// Run the prebuilt binary and leave with its exit status.
//
// The launcher is the process a terminal, a process manager or an MCP host
// signals, but the work happens in the binary it starts. A blocking spawn
// cannot pass a signal on, so killing the launcher left the binary running
// with init as its parent - `sessionwiki web` still holding its port. The
// binary is started asynchronously and every terminating signal the launcher
// receives is forwarded; the launcher exits only when the binary has.
const { spawn } = require("child_process");
const { describeExit } = require("./exit.js");

const FORWARDED = ["SIGINT", "SIGTERM", "SIGHUP"];

function run(bin, args) {
  const child = spawn(bin, args, { stdio: "inherit" });
  const forwarders = FORWARDED.map((signal) => {
    const forward = () => {
      // The binary may already be gone; nothing else to do then.
      try {
        child.kill(signal);
      } catch {}
    };
    process.on(signal, forward);
    return [signal, forward];
  });
  child.on("error", (error) => {
    console.error(
      `sessionwiki: failed to run the prebuilt binary (${error.message}). ` +
        "Try `cargo install sessionwiki`."
    );
    process.exit(1);
  });
  child.on("exit", (status, signal) => {
    for (const [name, forward] of forwarders) process.off(name, forward);
    const { code, note } = describeExit({ status, signal });
    if (note) console.error(note);
    process.exit(code);
  });
}

module.exports = { run };
