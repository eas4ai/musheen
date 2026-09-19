#!/usr/bin/env node

import { spawnSync } from "node:child_process";

const failures = [];
const trackedLockfile = spawnSync(
  "git",
  ["ls-files", "--error-unmatch", "Cargo.lock"],
  { stdio: "ignore" },
);
if (trackedLockfile.status !== 0) {
  failures.push("Cargo.lock is not committed");
}

const build = spawnSync("cargo", ["build", "--locked"], {
  stdio: "inherit",
});
if (build.error) failures.push(`the locked build could not run: ${build.error.message}`);
if (build.status !== 0) failures.push("cargo build --locked failed");

if (failures.length > 0) {
  console.log("cairn: DEP-015: fail");
  for (const failure of failures) console.error(`DEP-015: ${failure}`);
  process.exit(1);
}

console.log("cairn: DEP-015: pass");
