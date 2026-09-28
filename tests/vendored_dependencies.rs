//! The workspace uses GPUI Kit 0.7.x, the vendored GPUI crates start from
//! the versions it resolves, and docs/vendor.md lists every vendored crate
//! with the published version it starts from (DEP-023).

use std::fs;
use std::path::Path;

/// The GPUI crates GPUI Kit 0.7.0 pins, which the vendored copies start from.
const GPUI_KIT_0_7_PINS: [(&str, &str); 3] = [
    ("gpui-pre", "0.3.7"),
    ("gpui-pre-linux", "0.3.7"),
    ("gpui-component", "0.7.0"),
];

/// The versions the lockfile resolves for the package `name`.
fn locked_versions(lock: &str, name: &str) -> Vec<String> {
    let header = format!("name = \"{name}\"");
    let mut versions = Vec::new();
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line == header
            && let Some(version) = lines
                .next()
                .and_then(|line| line.strip_prefix("version = \""))
                .and_then(|version| version.strip_suffix('"'))
        {
            versions.push(version.to_owned());
        }
    }
    versions
}

/// Each crate the root manifest patches from a local path, with the version
/// its own manifest declares.
fn vendored_crates(root: &Path) -> Vec<(String, String)> {
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let patches = manifest
        .split("[patch.crates-io]")
        .nth(1)
        .expect("the manifest patches crates-io");
    patches
        .lines()
        .take_while(|line| !line.starts_with('['))
        .filter_map(|line| {
            let (name, source) = line.split_once('=')?;
            let path = source.split("path = \"").nth(1)?.split('"').next()?;
            let crate_manifest = fs::read_to_string(root.join(path).join("Cargo.toml"))
                .unwrap_or_else(|error| panic!("{path}/Cargo.toml: {error}"));
            let package = crate_manifest.split("[package]").nth(1)?;
            let version = package.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("version = \"")?
                    .strip_suffix('"')
                    .map(str::to_owned)
            })?;
            Some((name.trim().to_owned(), version))
        })
        .collect()
}

#[test]
fn vendored_dependencies_follow_gpui_kit_0_7() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lock = fs::read_to_string(root.join("Cargo.lock")).unwrap();
    let kit = locked_versions(&lock, "gpui-kit");
    assert!(
        !kit.is_empty() && kit.iter().all(|version| version.starts_with("0.7.")),
        "the lockfile resolves GPUI Kit {kit:?}"
    );
    let vendored = vendored_crates(root);
    for (name, pinned) in GPUI_KIT_0_7_PINS {
        let version = vendored
            .iter()
            .find_map(|(vendored, version)| (vendored == name).then_some(version.as_str()));
        assert_eq!(
            version,
            Some(pinned),
            "vendored {name} starts from the version GPUI Kit 0.7 resolves"
        );
    }
    let list = fs::read_to_string(root.join("docs/vendor.md"))
        .expect("docs/vendor.md lists the vendored crates");
    for (name, version) in &vendored {
        assert!(
            list.lines().any(|line| line.trim() == format!("## {name} {version}")),
            "docs/vendor.md has the heading `## {name} {version}`"
        );
    }
}
