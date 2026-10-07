// How the launcher reports the prebuilt binary's ending.
//
// `spawnSync` reports a signal death as `status: null` plus a signal name.
// Collapsing that to exit 1 would make "something killed sessionwiki" look
// like "sessionwiki failed", and a signal death prints nothing else.
const { constants } = require("os");

function describeExit(result) {
  if (typeof result.status === "number") {
    return { code: result.status, note: null };
  }
  const signal = result.signal;
  if (!signal) {
    return {
      code: 1,
      note:
        "sessionwiki: the binary ended without an exit code and without a signal. " +
        "Exiting 1, but that 1 is this launcher's, not the binary's.",
    };
  }
  const number = constants.signals[signal];
  return {
    // 128 + signal is the shell convention, so 137 reads as SIGKILL.
    code: number ? 128 + number : 1,
    note:
      `sessionwiki: the binary was killed by ${signal}. Something on this machine ` +
      "sent that signal - this is not sessionwiki exiting on its own.",
  };
}

module.exports = { describeExit };
