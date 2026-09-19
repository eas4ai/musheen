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

const candidate = spawnSync("git", ["rev-parse", "HEAD"], {
  encoding: "utf8",
}).stdout.trim();
const archive = spawnSync(
  "git",
  [
    "archive",
    "--format=tar",
    candidate,
    "--",
    "Cargo.toml",
    "Cargo.lock",
    "src",
    "vendor/native-theme-gpui",
    "ci/dep-015.Dockerfile",
  ],
  { maxBuffer: 50 * 1024 * 1024 },
);
if (archive.error) failures.push(`the committed candidate could not be archived: ${archive.error.message}`);
if (archive.status !== 0) failures.push("git archive failed for the committed candidate");

const build = archive.status === 0
  ? spawnSync(
      "docker",
      [
        "build",
        "--progress=plain",
        "--tag",
        `musheen-dep-015:${candidate.slice(0, 12)}`,
        "--file",
        "ci/dep-015.Dockerfile",
        "-",
      ],
      { input: archive.stdout, stdio: ["pipe", "inherit", "inherit"] },
    )
  : { status: 1 };
if (build.error) failures.push(`the Linux container build could not run: ${build.error.message}`);
if (build.status !== 0) failures.push("the committed tree failed cargo build --locked in Linux Docker");

const activeChecks = spawnSync(
  "docker",
  ["ps", "--filter", "ancestor=musheen-dep-015", "--format", "{{.ID}}"],
  { encoding: "utf8" },
);
if (activeChecks.status === 0 && activeChecks.stdout.trim()) {
  failures.push("a DEP-015 verification container was left running");
}

if (failures.length > 0) {
  console.log("cairn: DEP-015: fail");
  for (const failure of failures) console.error(`DEP-015: ${failure}`);
  process.exit(1);
}

console.log("cairn: DEP-015: pass");
