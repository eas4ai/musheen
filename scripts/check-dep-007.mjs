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
  ["nix", /^(?:\^|~)?0\.31(?:\.\d+)?$/],
  ["proc-mounts", /^(?:\^|~)?0\.3(?:\.\d+)?$/],
  ["xattr", /^(?:\^|~)?1(?:\.\d+(?:\.\d+)?)?$/],
  ["reflink-copy", /^(?:\^|~)?0\.1(?:\.\d+)?$/],
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
for (const { path, features } of declarations("nix")) {
  if (!features.includes("fs")) failures.push(`${path} declares nix without its fs feature`);
}

for (const name of ["libc", "libmount", "mountpoints", "procfs", "sys-mount"]) {
  for (const { path } of declarations(name)) {
    failures.push(`${path} directly declares forbidden competing package ${name}`);
  }
}

for (const path of tracked(":(glob)**/*.rs")) {
  const source = readFileSync(path, "utf8");
  const compactSource = source.replace(/[^A-Za-z0-9_.-]/g, "");
  if (/\blibc\s*::/.test(source) || /\bnix\s*::\s*libc\b/.test(source)) {
    failures.push(`${path} calls or imports libc directly`);
  }
  const mountPaths = [
    "/etc/mtab",
    "/proc/mounts",
    "/proc/self/mounts",
    "/proc/self/mountinfo",
  ];
  if (mountPaths.some((mountPath) =>
    source.includes(mountPath)
    || compactSource.includes(mountPath.replace(/[^A-Za-z0-9_.-]/g, "")))) {
    failures.push(`${path} parses the mount table directly`);
  }
  if (compactSource.includes("procself") && compactSource.includes("mountinfo")) {
    failures.push(`${path} assembles the mountinfo table path directly`);
  }
}

if (failures.length > 0) {
  console.log("cairn: DEP-007: fail");
  for (const failure of failures) console.error(`DEP-007: ${failure}`);
  process.exit(1);
}
console.log("cairn: DEP-007: pass");
