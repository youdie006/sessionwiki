#!/usr/bin/env node
// Build the platform packages from a release's prebuilt binaries and publish the
// whole set - the main package plus one os/cpu-restricted package per target.
// No install script anywhere: npm installs only the platform package matching
// the machine.
//
// Usage:
//   node publish.mjs [<version>] [--dry-run]
//     <version>   defaults to the version in Cargo.toml.
//     --dry-run   builds + `npm pack`s into ./build, publishes nothing.
//
// Binaries come from https://github.com/youdie006/sessionwiki/releases/download/
//   v<version>/sessionwiki-v<version>-<target>.(tar.gz|zip)
// The macOS binaries in them are already signed by the release workflow; they
// are copied byte for byte, so the signature survives.

import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import {
  mkdirSync,
  writeFileSync,
  chmodSync,
  rmSync,
  readFileSync,
  copyFileSync,
  existsSync,
} from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const mainPkgPath = join(here, "package.json");
const mainPkg = JSON.parse(readFileSync(mainPkgPath, "utf8"));

const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");

// Cargo.toml is the one place the release version is declared; this
// package.json is rewritten below on every publish.
function cargoVersion() {
  const toml = readFileSync(join(here, "..", "Cargo.toml"), "utf8");
  const m = toml.match(/^version\s*=\s*"([^"]+)"/m);
  if (!m) throw new Error("no version found in Cargo.toml");
  return m[1];
}
const version = args.find((a) => !a.startsWith("--")) || cargoVersion();
const SCOPE = "@youdie006";

const PLATFORMS = [
  { pkg: "sessionwiki-darwin-arm64", os: "darwin", cpu: "arm64", target: "aarch64-apple-darwin", exe: "sessionwiki", archive: "tar.gz" },
  { pkg: "sessionwiki-darwin-x64", os: "darwin", cpu: "x64", target: "x86_64-apple-darwin", exe: "sessionwiki", archive: "tar.gz" },
  { pkg: "sessionwiki-linux-x64", os: "linux", cpu: "x64", target: "x86_64-unknown-linux-gnu", exe: "sessionwiki", archive: "tar.gz" },
  { pkg: "sessionwiki-win32-x64", os: "win32", cpu: "x64", target: "x86_64-pc-windows-msvc", exe: "sessionwiki.exe", archive: "zip" },
];

const sh = (cmd, cmdArgs, opts = {}) =>
  execFileSync(cmd, cmdArgs, { stdio: "inherit", ...opts });

const buildDir = join(here, "build");
rmSync(buildDir, { recursive: true, force: true });
mkdirSync(buildDir, { recursive: true });

// Keep the published main README in sync with the repo root README.
const rootReadme = join(here, "..", "README.md");
if (existsSync(rootReadme)) copyFileSync(rootReadme, join(here, "README.md"));

// 1) Build each platform package: the binary + an os/cpu-restricted manifest.
for (const p of PLATFORMS) {
  const dir = join(buildDir, p.pkg);
  mkdirSync(join(dir, "bin"), { recursive: true });
  const stem = `sessionwiki-v${version}-${p.target}`;
  const url = `https://github.com/youdie006/sessionwiki/releases/download/v${version}/${stem}.${p.archive}`;
  const archive = join(buildDir, `${stem}.${p.archive}`);
  sh("curl", ["-fSL", "-o", archive, url]);
  if (p.archive === "zip") {
    sh("unzip", ["-q", "-o", archive, "-d", buildDir]);
  } else {
    sh("tar", ["-xzf", archive, "-C", buildDir]);
  }
  const extracted = join(buildDir, stem, p.exe);
  if (!existsSync(extracted)) throw new Error(`archive for ${p.target} did not contain ${p.exe}`);
  const bin = join(dir, "bin", p.exe);
  copyFileSync(extracted, bin);
  chmodSync(bin, 0o755);
  rmSync(archive);
  rmSync(join(buildDir, stem), { recursive: true, force: true });
  const manifest = {
    name: `${SCOPE}/${p.pkg}`,
    version,
    description: `Prebuilt sessionwiki binary for ${p.os}-${p.cpu}. Installed automatically by ${SCOPE}/sessionwiki; do not depend on it directly.`,
    license: mainPkg.license,
    repository: mainPkg.repository,
    homepage: mainPkg.homepage,
    os: [p.os],
    cpu: [p.cpu],
    files: [`bin/${p.exe}`],
  };
  writeFileSync(join(dir, "package.json"), JSON.stringify(manifest, null, 2) + "\n");
}

// 2) Pin the main package's version + optionalDependencies to this version.
mainPkg.version = version;
mainPkg.optionalDependencies = Object.fromEntries(
  PLATFORMS.map((p) => [`${SCOPE}/${p.pkg}`, version])
);
writeFileSync(mainPkgPath, JSON.stringify(mainPkg, null, 2) + "\n");

// 3) Publish platform packages FIRST (so the main's optionalDependencies
//    resolve on the registry), then the main. --dry-run packs instead.
const npmArgs = dryRun
  ? ["pack", "--pack-destination", buildDir]
  : ["publish", "--access", "public"];
for (const p of PLATFORMS) sh("npm", npmArgs, { cwd: join(buildDir, p.pkg) });
sh("npm", npmArgs, { cwd: here });

// Wait for the registry to SERVE what was just published: `npm publish`
// returning means the write was accepted, not that an install can resolve it.
const { publishSummary, wantedSpecs } = createRequire(import.meta.url)("./summary.js");
const unresolved = [];
if (!dryRun) {
  for (const spec of wantedSpecs(SCOPE, PLATFORMS, version)) {
    const deadline = Date.now() + 120_000;
    for (;;) {
      let seen = "";
      try {
        seen = execFileSync("npm", ["view", spec, "version"], {
          encoding: "utf8",
          stdio: ["ignore", "pipe", "ignore"],
        }).trim();
      } catch {
        seen = "";
      }
      if (seen === version) break;
      if (Date.now() > deadline) {
        unresolved.push(spec);
        break;
      }
      execFileSync("sleep", ["3"], { stdio: "ignore" });
    }
  }
}

if (dryRun) {
  console.log(`\nDRY RUN complete: tarballs in ${buildDir}, nothing published.`);
} else {
  const { text, ok } = publishSummary(version, PLATFORMS.length, unresolved);
  console.log(text);
  if (!ok) process.exit(1);
}
