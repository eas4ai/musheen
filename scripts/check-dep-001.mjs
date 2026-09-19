#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const tracked = (...pathspecs) =>
  execFileSync("git", ["ls-files", "-z", "--", ...pathspecs], {
    encoding: "utf8",
  })
    .split("\0")
    .filter(Boolean);

const manifests = tracked(":(glob)**/Cargo.toml");
const rustSources = tracked(":(glob)**/*.rs");
const failures = [];

const dependencyLine = (name) =>
  new RegExp(
    `^\\s*${name.replaceAll("-", "\\-")}\\s*=\\s*(?:"([^"]+)"|\\{([^}]*)\\})\\s*$`,
    "m",
  );

const declaredVersions = (name) => {
  const versions = [];
  for (const path of manifests) {
    const match = dependencyLine(name).exec(readFileSync(path, "utf8"));
    if (!match) continue;
    if (match[1]) versions.push({ path, version: match[1] });
    if (match[2]) {
      const version = /\bversion\s*=\s*"([^"]+)"/.exec(match[2])?.[1];
      if (version) versions.push({ path, version });
    }
  }
  return versions;
};

const isCompatible05 = (version) => /^(?:\^|~)?0\.5(?:\.\d+)?$/.test(version);

for (const name of ["native-theme", "native-theme-gpui"]) {
  const versions = declaredVersions(name);
  if (versions.length === 0) {
    failures.push(`${name} is not declared with a concrete version`);
    continue;
  }
  for (const { path, version } of versions) {
    if (!isCompatible05(version)) {
      failures.push(`${path} declares ${name} ${version}, not the 0.5.x line`);
    }
  }
}

const lock = readFileSync("Cargo.lock", "utf8");
for (const name of ["native-theme", "native-theme-gpui"]) {
  const block = new RegExp(
    `\\[\\[package\\]\\]\\nname = "${name}"\\nversion = "([^"]+)"`,
  ).exec(lock);
  if (!block) {
    failures.push(`Cargo.lock does not contain ${name}`);
  } else if (!/^0\.5\./.test(block[1])) {
    failures.push(`Cargo.lock resolves ${name} ${block[1]}, not 0.5.x`);
  }
}

for (const name of ["dark-light", "system-theme", "system_theme"]) {
  const direct = dependencyLine(name);
  const alias = new RegExp(`\\bpackage\\s*=\\s*"${name}"`);
  for (const path of manifests) {
    const manifest = readFileSync(path, "utf8");
    if (direct.test(manifest) || alias.test(manifest)) {
      failures.push(`${path} declares competing appearance provider ${name}`);
    }
  }
  if (new RegExp(`\\[\\[package\\]\\]\\nname = "${name}"\\n`).test(lock)) {
    failures.push(`Cargo.lock contains competing appearance provider ${name}`);
  }
}

const forbiddenAppearanceReads = [
  ["org.freedesktop.portal.Settings", "direct portal Settings access"],
  ["org.freedesktop.appearance", "direct appearance-portal access"],
  ["gtk-theme-name", "direct GTK theme-file access"],
  ["gtk-3.0/settings.ini", "direct GTK 3 settings-file access"],
  ["gtk-4.0/settings.ini", "direct GTK 4 settings-file access"],
  ["kdeglobals", "direct KDE theme-file access"],
  ["GTK_THEME", "direct GTK_THEME environment access"],
  ["gsettings", "direct gsettings theme access"],
];
const fragmentedAppearanceReads = [
  [["gtk-3.0", "settings.ini"], "direct GTK 3 settings-file access"],
  [["gtk-4.0", "settings.ini"], "direct GTK 4 settings-file access"],
];

for (const path of rustSources) {
  const source = readFileSync(path, "utf8");
  const compactSource = source.replace(/[^A-Za-z0-9_.-]/g, "");
  for (const [needle, description] of forbiddenAppearanceReads) {
    const compactNeedle = needle.replace(/[^A-Za-z0-9_.-]/g, "");
    if (source.includes(needle) || compactSource.includes(compactNeedle)) {
      failures.push(`${path}: ${description}`);
    }
  }
  for (const [needles, description] of fragmentedAppearanceReads) {
    if (needles.every((needle) => compactSource.includes(needle))) {
      failures.push(`${path}: ${description}`);
    }
  }
}

if (failures.length > 0) {
  console.log("cairn: DEP-001: fail");
  for (const failure of failures) console.error(`DEP-001: ${failure}`);
  process.exit(1);
}

console.log("cairn: DEP-001: pass");
