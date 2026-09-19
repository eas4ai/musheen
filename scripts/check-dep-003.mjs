#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const manifestPaths = execFileSync(
  "git",
  ["ls-files", "-z", "--", ":(glob)**/Cargo.toml"],
  { encoding: "utf8" },
)
  .split("\0")
  .filter(Boolean);
const manifests = manifestPaths.map((path) => ({
  path,
  text: readFileSync(path, "utf8"),
}));
const rustPaths = execFileSync(
  "git",
  ["ls-files", "-z", "--", ":(glob)**/*.rs"],
  { encoding: "utf8" },
)
  .split("\0")
  .filter(Boolean);
const failures = [];

const dependency = (name) => {
  const escaped = name.replaceAll("-", "\\-");
  const expression = new RegExp(
    `^\\s*${escaped}\\s*=\\s*(?:"([^"]+)"|\\{([^}]*)\\})\\s*$`,
    "m",
  );
  return manifests.flatMap(({ path, text }) => {
    const match = expression.exec(text);
    if (!match) return [];
    const version =
      match[1] ?? /\bversion\s*=\s*"([^"]+)"/.exec(match[2])?.[1];
    return [{ path, version, details: match[2] ?? "" }];
  });
};

const selected = dependency("freedesktop");
if (selected.length === 0) {
  failures.push("freedesktop is not declared with a concrete version");
}
for (const { path, version, details } of selected) {
  if (!/^(?:\^|~)?0\.0(?:\.\d+)?$/.test(version ?? "")) {
    failures.push(`${path} declares freedesktop ${version ?? "without a version"}`);
  }
  if (/default-features\s*=\s*false/.test(details)) {
    for (const feature of ["core", "apps", "icon"]) {
      if (!new RegExp(`"${feature}"`).test(details)) {
        failures.push(`${path} disables defaults without the ${feature} feature`);
      }
    }
  }
}

const lock = readFileSync("Cargo.lock", "utf8");
for (const name of [
  "freedesktop",
  "freedesktop-core",
  "freedesktop-apps",
  "freedesktop-icon",
]) {
  const match = new RegExp(
    `\\[\\[package\\]\\]\\nname = "${name}"\\nversion = "([^"]+)"`,
  ).exec(lock);
  if (!match) failures.push(`Cargo.lock does not contain ${name}`);
  else if (!/^0\.0\./.test(match[1])) {
    failures.push(`Cargo.lock resolves ${name} ${match[1]}, not 0.0.x`);
  }
}

const competingPackages = [
  "freedesktop-desktop-entry",
  "freedesktop_entry_parser",
  "freedesktop-file-parser",
  "freedesktop-icon-lookup",
  "freedesktop-icons",
  "freedesktop-icons-greedy",
  "linicon",
];
for (const name of competingPackages) {
  for (const { path } of dependency(name)) {
    failures.push(`${path} directly declares competing package ${name}`);
  }
  const alias = new RegExp(`\\bpackage\\s*=\\s*"${name}"`);
  for (const { path, text } of manifests) {
    if (alias.test(text)) failures.push(`${path} aliases competing package ${name}`);
  }
  if (new RegExp(`\\[\\[package\\]\\]\\nname = "${name}"\\n`).test(lock)) {
    failures.push(`Cargo.lock contains competing package ${name}`);
  }
}

const rustSources = rustPaths.map((path) => readFileSync(path, "utf8"));
const compactTreeSource = rustSources
  .join("\n")
  .replace(/[^A-Za-z0-9]/g, "")
  .toLowerCase();
const traversesTextLines = rustSources.some((source) =>
  /\.lines\s*\(|\.(?:split|split_terminator)\s*\(\s*['"]\\n['"]/.test(source));
const splitsKeyValue = rustSources.some((source) =>
  /split_once\s*\(\s*['"]=['"]|splitn\s*\(\s*\d+\s*,\s*['"]=['"]|\.split\s*\(\s*['"]=['"]/.test(source));
if (compactTreeSource.includes("desktopentry") && traversesTextLines && splitsKeyValue) {
  failures.push("Rust source tree contains a hand-written Desktop Entry parser");
}
const usesFilesystemPaths = rustSources.some((source) =>
  /\bstd\s*::\s*path\b|\bPath(?:Buf)?\s*::|\.join\s*\(/.test(source));
if (compactTreeSource.includes("hicolor") && usesFilesystemPaths) {
  failures.push("Rust source tree contains a hand-written icon-theme resolver");
}

if (failures.length > 0) {
  console.log("cairn: DEP-003: fail");
  for (const failure of failures) console.error(`DEP-003: ${failure}`);
  process.exit(1);
}

console.log("cairn: DEP-003: pass");
