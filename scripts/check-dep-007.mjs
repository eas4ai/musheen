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
  ["nix", /^(?:\^|~)?0\.31(?:\.\d+)?$/],
  ["proc-mounts", /^(?:\^|~)?0\.3(?:\.\d+)?$/],
  ["xattr", /^(?:\^|~)?1(?:\.\d+(?:\.\d+)?)?$/],
  ["reflink-copy", /^(?:\^|~)?0\.1(?:\.\d+)?$/],
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
for (const { path, details } of declarations("nix")) {
  if (!/"fs"/.test(details)) failures.push(`${path} declares nix without its fs feature`);
}

for (const name of ["libc", "libmount", "mountpoints", "procfs", "sys-mount"]) {
  for (const { path } of declarations(name)) {
    failures.push(`${path} directly declares forbidden competing package ${name}`);
  }
  const alias = new RegExp(`\\bpackage\\s*=\\s*"${name}"`);
  for (const { path, text } of manifests) {
    if (alias.test(text)) {
      failures.push(`${path} aliases forbidden competing package ${name}`);
    }
  }
}

for (const path of tracked(":(glob)**/*.rs")) {
  const source = readFileSync(path, "utf8");
  if (/\blibc::/.test(source)) failures.push(`${path} calls libc directly`);
  if (source.includes('"/proc/mounts"') || source.includes('"/proc/self/mounts"')) {
    failures.push(`${path} parses the mount table directly`);
  }
}

if (failures.length > 0) {
  console.log("cairn: DEP-007: fail");
  for (const failure of failures) console.error(`DEP-007: ${failure}`);
  process.exit(1);
}
console.log("cairn: DEP-007: pass");
