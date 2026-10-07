// What a publish run is allowed to claim at the end. A release where one
// platform package has not landed must not report success: the next `npm i -g`
// would remove the working platform package and install nothing in its place.
function publishSummary(version, total, unresolved) {
  if (unresolved.length === 0) {
    return {
      text: `\nPublished sessionwiki ${version}: main + ${total} platform packages, all resolvable.`,
      ok: true,
    };
  }
  return {
    text:
      `\nPublished sessionwiki ${version}, but ${unresolved.length} of ${total + 1} package(s) ` +
      `are NOT resolvable yet:\n` +
      unresolved.map((s) => `  ${s}`).join("\n") +
      `\nInstalling this version now can remove a working platform package and ` +
      `install nothing in its place. Wait for the registry and re-check with ` +
      `\`npm view <spec> version\` before installing.`,
    ok: false,
  };
}

// Every package spec the resolvability check must ask npm about - scoped, all
// of them, exactly as they were published.
function wantedSpecs(scope, platforms, version) {
  return [
    `${scope}/sessionwiki@${version}`,
    ...platforms.map((p) => `${scope}/${p.pkg}@${version}`),
  ];
}

module.exports = { publishSummary, wantedSpecs };
