#!/usr/bin/env node
// Resolve the prebuilt binary from the platform-specific optionalDependency and
// run it, forwarding argv, stdio, and the exit code. There is no install
// script: npm installs only the optionalDependency whose `os`/`cpu` match this
// machine, so nothing runs at install time.
//
// npm upgrades the binary in place at the same path, and macOS binaries are
// signed with one stable identity, so a folder-access grant macOS asked for
// once still applies after an upgrade.

const { run } = require("./run.js");

const PKGS = {
  "darwin arm64": "@youdie006/sessionwiki-darwin-arm64",
  "darwin x64": "@youdie006/sessionwiki-darwin-x64",
  "linux x64": "@youdie006/sessionwiki-linux-x64",
  "win32 x64": "@youdie006/sessionwiki-win32-x64",
};

function binaryPath() {
  const pkg = PKGS[`${process.platform} ${process.arch}`];
  if (!pkg) return null;
  const file = process.platform === "win32" ? "sessionwiki.exe" : "sessionwiki";
  try {
    return require.resolve(`${pkg}/bin/${file}`);
  } catch {
    return null;
  }
}

const bin = binaryPath();
if (!bin) {
  console.error(
    `sessionwiki: no prebuilt binary for ${process.platform} ${process.arch} ` +
      "(prebuilt for macOS arm64/x64, Linux x64 and Windows x64). " +
      "If your platform is one of those, the optional dependency failed to install - " +
      "reinstall, or use `cargo install sessionwiki`."
  );
  process.exit(1);
}

run(bin, process.argv.slice(2));
