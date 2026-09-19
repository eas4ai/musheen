#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const tracked = (...pathspecs) =>
  execFileSync("git", ["ls-files", "-z", "--", ...pathspecs], {
    encoding: "utf8",
  })
    .split("\0")
    .filter(Boolean);
const metadata = JSON.parse(execFileSync(
  "cargo",
  ["metadata", "--locked", "--no-deps", "--format-version", "1"],
  { encoding: "utf8" },
));
const workspaceMembers = new Set(metadata.workspace_members);
const dependencies = metadata.packages
  .filter(({ id }) => workspaceMembers.has(id))
  .flatMap(({ dependencies: packageDependencies, manifest_path: path }) =>
    packageDependencies
      .filter(({ kind }) => kind === null)
      .map((dependency) => ({ ...dependency, path }))
  );
const lock = readFileSync("Cargo.lock", "utf8");
const failures = [];

const declarations = (name) => dependencies.filter((dependency) => dependency.name === name);

const required = [
  ["walkdir", /^(?:\^|~)?2(?:\.\d+(?:\.\d+)?)?$/],
  ["rustix", /^(?:\^|~)?1(?:\.\d+(?:\.\d+)?)?$/],
  ["open", /^(?:\^|~)?5(?:\.\d+(?:\.\d+)?)?$/],
  ["camino", /^(?:\^|~)?1(?:\.\d+(?:\.\d+)?)?$/],
  ["wax", /^(?:\^|~)?0\.7(?:\.\d+)?$/],
  ["notify", /^(?:\^|~)?8(?:\.\d+(?:\.\d+)?)?$/],
];
for (const [name, versionPattern] of required) {
  const found = declarations(name);
  if (found.length === 0) failures.push(`${name} is not declared`);
  for (const { path, req } of found) {
    if (!versionPattern.test(req ?? "")) {
      failures.push(`${path} declares ${name} ${req ?? "without a version"}`);
    }
  }
  const lockedVersions = [...lock.matchAll(
    new RegExp(`\\[\\[package\\]\\]\\nname = "${name}"\\nversion = "([^"]+)"`, "g"),
  )].map((match) => match[1]);
  if (!lockedVersions.some((version) => versionPattern.test(version))) {
    failures.push(`Cargo.lock does not resolve an approved ${name} version`);
  }
}
for (const { path, features } of declarations("rustix")) {
  if (!features.includes("fs")) failures.push(`${path} declares rustix without its fs feature`);
}

const approvedDirectPackages = new Set([
  "camino",
  "freedesktop",
  "native-theme",
  "native-theme-gpui",
  "nix",
  "notify",
  "open",
  "proc-mounts",
  "reflink-copy",
  "rustix",
  "walkdir",
  "wax",
  "xattr",
]);
for (const { name, path } of dependencies) {
  if (!approvedDirectPackages.has(name)) {
    failures.push(`${path} declares unreviewed direct package ${name}`);
  }
}

// Packages internal to an approved dependency remain transitive implementation
// details. Any new app-selected runtime dependency requires a reviewed update
// to this foundation allowlist.

for (const path of tracked(":(glob)**/*.rs")) {
  const source = readFileSync(path, "utf8");
  if (!/\bUtf8Path(?:Buf)?\b/.test(source)) continue;
  const aliasesCaminoType = /\btype\s+\w+\s*=\s*[^;]*\bUtf8Path(?:Buf)?\b/.test(source)
    || /\b(?:pub\s+)?use\s+camino\b[^;]*\bas\s+\w+/.test(source);
  const wrapsCaminoType = /\b(?:pub(?:\([^)]*\))?\s+)?struct\s+\w+(?:\s*<[^>{;]*>)?\s*(?:\([^;]*\bUtf8Path(?:Buf)?\b[^;]*\)\s*;|\{[^}]*\bUtf8Path(?:Buf)?\b[^}]*\})/s.test(source)
    || /\b(?:pub(?:\([^)]*\))?\s+)?enum\s+\w+(?:\s*<[^>{;]*>)?\s*\{[^}]*\bUtf8Path(?:Buf)?\b[^}]*\}/s.test(source);
  const convertsLossyLocalPath = /\bto_string_lossy\s*\(/.test(source);
  if (convertsLossyLocalPath) {
    failures.push(`${path} converts a lossy local path into a Camino path`);
    continue;
  }
  if (aliasesCaminoType || wrapsCaminoType) {
    failures.push(`${path} exports a Camino path type across module boundaries`);
    continue;
  }
  if (!/(?:^|\/)(?:config|uri)(?:\/|\.rs$)/.test(path)) {
    failures.push(`${path} uses Camino outside a configuration or URI boundary`);
  }
}

if (failures.length > 0) {
  console.log("cairn: DEP-008: fail");
  for (const failure of failures) console.error(`DEP-008: ${failure}`);
  process.exit(1);
}
console.log("cairn: DEP-008: pass");
