#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";

const failures = [];
const trackedLockfile = spawnSync(
  "git",
  ["ls-files", "--error-unmatch", "Cargo.lock"],
  { stdio: "ignore" },
);
if (trackedLockfile.status !== 0) {
  failures.push("Cargo.lock is not committed");
}

const workflow = readFileSync(".github/workflows/ci.yml", "utf8");
const workflowLines = workflow.split("\n");
for (const trigger of ["push", "pull_request"]) {
  const triggerIndex = workflowLines.findIndex((line) => line === `  ${trigger}:`);
  if (triggerIndex < 0) {
    failures.push(`the CI workflow does not run on ${trigger}`);
    continue;
  }
  const nextContent = workflowLines
    .slice(triggerIndex + 1)
    .find((line) => line.trim().length > 0);
  if (nextContent?.startsWith("    ")) {
    failures.push(`the CI ${trigger} trigger is filtered`);
  }
}
const jobStart = workflowLines.findIndex((line) => line === "  locked-build:");
const jobEnd = workflowLines.findIndex(
  (line, index) => index > jobStart && /^  [\w-]+:\s*$/.test(line),
);
const lockedJob = jobStart < 0
  ? ""
  : workflowLines.slice(jobStart, jobEnd < 0 ? undefined : jobEnd).join("\n");
if (!lockedJob) {
  failures.push("the CI workflow has no locked-build job");
} else {
  if (!/^\s*run:\s*cargo build --locked\s*$/m.test(lockedJob)) {
    failures.push("the locked-build job does not run cargo build --locked");
  }
  if (/^\s*if\s*:/m.test(lockedJob)) {
    failures.push("the locked-build job or one of its steps is conditional");
  }
  if (/^\s*continue-on-error:\s*true\s*$/m.test(lockedJob)) {
    failures.push("the locked-build job suppresses a build failure");
  }
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
