//! The workspace uses GPUI Kit 0.7.x, the vendored GPUI crates start from
//! the versions it resolves, and docs/vendor.md lists every vendored crate
//! with the published version it starts from (DEP-023).

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// The GPUI crates Musheen vendors, as DEP-023 names them.
const VENDORED_GPUI_CRATES: [&str; 3] = ["gpui-pre", "gpui-pre-linux", "gpui-component"];

/// One `[[package]]` entry of Cargo.lock.
struct LockedPackage {
    name: String,
    version: String,
    /// `None` for a path package, such as a vendored copy.
    source: Option<String>,
    dependencies: Vec<String>,
}

fn locked_packages(lock: &str) -> Vec<LockedPackage> {
    lock.split("\n[[package]]\n")
        .skip(1)
        .map(|block| {
            // The last package may be followed by another table, such as
            // `[[patch.unused]]`.
            let block = block.split("\n[[").next().unwrap_or(block);
            let value = |key: &str| {
                block.lines().find_map(|line| {
                    line.strip_prefix(key)?
                        .strip_prefix(" = \"")?
                        .strip_suffix('"')
                        .map(str::to_owned)
                })
            };
            let dependencies = block
                .split("dependencies = [")
                .nth(1)
                .map(|list| {
                    list.split(']')
                        .next()
                        .unwrap_or_default()
                        .lines()
                        .filter_map(|line| {
                            let line = line.trim();
                            let line = line.strip_suffix(',').unwrap_or(line);
                            line.strip_prefix('"')?.strip_suffix('"').map(str::to_owned)
                        })
                        .collect()
                })
                .unwrap_or_default();
            LockedPackage {
                name: value("name").unwrap_or_else(|| panic!("a lock entry has no name: {block}")),
                version: value("version")
                    .unwrap_or_else(|| panic!("a lock entry has no version: {block}")),
                source: value("source"),
                dependencies,
            }
        })
        .collect()
}

/// The package a lock dependency entry (`name`, `name version` or
/// `name version (source)`) refers to.
fn resolve<'a>(packages: &'a [LockedPackage], dependency: &str) -> &'a LockedPackage {
    let (named, source) = match dependency.split_once(" (") {
        Some((named, source)) => (named, source.strip_suffix(')')),
        None => (dependency, None),
    };
    let mut parts = named.split(' ');
    let name = parts.next().unwrap_or_default();
    let version = parts.next();
    let matches = packages
        .iter()
        .filter(|package| {
            package.name == name
                && version.is_none_or(|version| package.version == version)
                && source.is_none_or(|source| package.source.as_deref() == Some(source))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "Cargo.lock names exactly one package for the dependency `{dependency}`"
    );
    matches[0]
}

/// Every package GPUI Kit resolves, directly or through its dependencies.
fn resolved_by<'a>(packages: &'a [LockedPackage], root: &str) -> Vec<&'a LockedPackage> {
    let mut pending = packages
        .iter()
        .filter(|package| package.name == root)
        .collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    let mut resolved = Vec::new();
    while let Some(package) = pending.pop() {
        if !seen.insert((package.name.as_str(), package.version.as_str())) {
            continue;
        }
        resolved.push(package);
        pending.extend(
            package
                .dependencies
                .iter()
                .map(|dependency| resolve(packages, dependency)),
        );
    }
    resolved
}

/// Each crate the root manifest patches from a local path, with the version
/// its own manifest declares. A patch or version this cannot read fails the
/// test instead of being skipped.
fn vendored_crates(root: &Path) -> Vec<(String, String)> {
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(
        !manifest.contains("[patch.crates-io."),
        "write each patch as one `name = {{ path = \"...\" }}` line under [patch.crates-io]"
    );
    let patches = manifest
        .split("\n[patch.crates-io]\n")
        .nth(1)
        .expect("the manifest patches crates-io");
    patches
        .lines()
        .take_while(|line| !line.starts_with('['))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (name, source) = line
                .split_once('=')
                .unwrap_or_else(|| panic!("cannot read the patch line `{line}`"));
            let path = source
                .split("path = \"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap_or_else(|| panic!("the patch `{line}` names no local path"));
            let crate_manifest = fs::read_to_string(root.join(path).join("Cargo.toml"))
                .unwrap_or_else(|error| panic!("{path}/Cargo.toml: {error}"));
            let package = crate_manifest
                .split("[package]")
                .nth(1)
                .unwrap_or_else(|| panic!("{path}/Cargo.toml has no [package] table"));
            let package = package.split("\n[").next().unwrap_or(package);
            let version = package
                .lines()
                .find_map(|line| {
                    line.trim()
                        .strip_prefix("version = \"")?
                        .strip_suffix('"')
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| panic!("{path}/Cargo.toml declares no literal package version"));
            (name.trim().to_owned(), version)
        })
        .collect()
}

#[test]
fn vendored_dependencies_follow_gpui_kit_0_7() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lock = fs::read_to_string(root.join("Cargo.lock")).unwrap();
    let packages = locked_packages(&lock);
    let kit = packages
        .iter()
        .filter(|package| package.name == "gpui-kit")
        .map(|package| package.version.as_str())
        .collect::<Vec<_>>();
    assert!(
        !kit.is_empty() && kit.iter().all(|version| version.starts_with("0.7.")),
        "the lockfile resolves GPUI Kit {kit:?}"
    );

    // Cargo records a patch whose version the dependents do not accept as
    // unused and builds the crates.io copy instead.
    assert!(
        !lock.contains("[[patch.unused]]"),
        "Cargo.lock lists a patch that the build does not use"
    );
    let vendored = vendored_crates(root);
    for (name, version) in &vendored {
        let locked = packages
            .iter()
            .filter(|package| &package.name == name)
            .collect::<Vec<_>>();
        assert!(!locked.is_empty(), "Cargo.lock uses vendored {name}");
        for package in locked {
            assert!(
                package.source.is_none() && &package.version == version,
                "Cargo.lock resolves {name} {} from {:?}, not the vendored {version}",
                package.version,
                package.source
            );
        }
    }

    // The version of each vendored GPUI crate is the one GPUI Kit resolves.
    let resolved = resolved_by(&packages, "gpui-kit");
    for name in VENDORED_GPUI_CRATES {
        let vendored_version = vendored
            .iter()
            .find_map(|(vendored, version)| (vendored == name).then_some(version))
            .unwrap_or_else(|| panic!("{name} is vendored"));
        let by_kit = resolved
            .iter()
            .filter(|package| package.name == name)
            .collect::<Vec<_>>();
        assert!(!by_kit.is_empty(), "GPUI Kit resolves {name}");
        for package in by_kit {
            assert!(
                package.source.is_none() && &package.version == vendored_version,
                "GPUI Kit resolves {name} {} from {:?}, not the vendored {vendored_version}",
                package.version,
                package.source
            );
        }
    }

    // docs/vendor.md has one section per vendored crate, headed by the
    // version it starts from, and no section at any other version.
    let list = fs::read_to_string(root.join("docs/vendor.md"))
        .expect("docs/vendor.md lists the vendored crates");
    let headings = list
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .map(str::trim)
        .collect::<Vec<_>>();
    for (name, version) in &vendored {
        let expected = format!("{name} {version}");
        let sections = headings
            .iter()
            .copied()
            .filter(|heading| heading.split(' ').next() == Some(name.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            sections,
            [expected.as_str()],
            "docs/vendor.md has one section for {name}, headed `## {expected}`"
        );
    }
    for heading in &headings {
        assert!(
            vendored
                .iter()
                .any(|(name, version)| *heading == format!("{name} {version}")),
            "the docs/vendor.md section `## {heading}` names no vendored crate at its version"
        );
    }
}
