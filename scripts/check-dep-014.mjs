#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";

const failures = [];
const provenance = readFileSync("vendor/native-theme-gpui/README.md", "utf8");
if (!provenance.includes("native-theme-gpui")) {
  failures.push("the vendored native-theme-gpui provenance is missing");
}

for (const [path, marker] of [
  ["vendor/native-theme-gpui/LICENSE-0BSD", "Permission to use, copy, modify"],
  ["vendor/native-theme-gpui/LICENSE-APACHE", "Apache License"],
  ["vendor/native-theme-gpui/LICENSE-MIT", "MIT License"],
]) {
  if (!readFileSync(path, "utf8").includes(marker)) {
    failures.push(`${path} does not contain the expected license text`);
  }
}

const audit = spawnSync("cargo", ["deny", "check", "licenses"], {
  stdio: "inherit",
});
if (audit.error) failures.push(`cargo-deny could not run: ${audit.error.message}`);
if (audit.status !== 0) failures.push("cargo-deny rejected the dependency licenses");

if (failures.length > 0) {
  console.log("cairn: DEP-014: fail");
  for (const failure of failures) console.error(`DEP-014: ${failure}`);
  process.exit(1);
}

console.log("cairn: DEP-014: pass");
