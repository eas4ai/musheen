#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const tracked = (...pathspecs) =>
  execFileSync("git", ["ls-files", "-z", "--", ...pathspecs], {
    encoding: "utf8",
  })
    .split("\0")
    .filter(Boolean);
const manifests = tracked(":(glob)**/Cargo.toml").map((path) => ({
  path,
  text: readFileSync(path, "utf8"),
}));
const failures = [];

const declarations = (name) => {
  const expression = new RegExp(
    `^\\s*${name.replaceAll("-", "\\-")}\\s*=\\s*(?:"([^"]+)"|\\{([^}]*)\\})\\s*$`,
    "m",
  );
  return manifests.flatMap(({ path, text }) => {
    const match = expression.exec(text);
    if (!match) return [];
    return [{
      path,
      version: match[1] ?? /\bversion\s*=\s*"([^"]+)"/.exec(match[2])?.[1],
      details: match[2] ?? "",
    }];
  });
};

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
  for (const { path, version } of found) {
    if (!versionPattern.test(version ?? "")) {
      failures.push(`${path} declares ${name} ${version ?? "without a version"}`);
    }
  }
}
for (const { path, details } of declarations("rustix")) {
  if (!/"fs"/.test(details)) failures.push(`${path} declares rustix without its fs feature`);
}

for (const name of [
  "fs_extra",
  "glob",
  "globset",
  "jwalk",
  "notify-debouncer-full",
  "notify-debouncer-mini",
  "opener",
]) {
  for (const { path } of declarations(name)) {
    failures.push(`${path} directly declares competing package ${name}`);
  }
  const alias = new RegExp(`\\bpackage\\s*=\\s*"${name}"`);
  for (const { path, text } of manifests) {
    if (alias.test(text)) {
      failures.push(`${path} aliases competing package ${name}`);
    }
  }
}

for (const path of tracked(":(glob)**/*.rs")) {
  const source = readFileSync(path, "utf8");
  if (!/camino::Utf8Path(?:Buf)?/.test(source)) continue;
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
