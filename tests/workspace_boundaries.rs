use std::fs;
use std::path::{Path, PathBuf};

const REQUIRED_CRATES: [&str; 6] = [
    "musheen-core",
    "musheen-local",
    "musheen-ops",
    "musheen-desktop",
    "musheen-ui",
    "musheen-test-support",
];

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files_below(path: &Path, files: &mut Vec<PathBuf>) {
    if !path.exists() {
        return;
    }

    for entry in fs::read_dir(path).expect("workspace source directory should be readable") {
        let entry = entry.expect("workspace source entry should be readable");
        let path = entry.path();
        if path.is_dir() {
            rust_files_below(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn workspace_contains_each_domain_crate() {
    let root = repository_root();
    let manifest =
        fs::read_to_string(root.join("Cargo.toml")).expect("workspace manifest should be readable");

    for crate_name in REQUIRED_CRATES {
        let crate_root = root.join("crates").join(crate_name);
        assert!(
            crate_root.join("Cargo.toml").is_file(),
            "{crate_name} must have a manifest"
        );
        assert!(
            crate_root.join("src/lib.rs").is_file(),
            "{crate_name} must have a library root"
        );
        assert!(
            manifest.contains(&format!("\"crates/{crate_name}\"")),
            "the root workspace must include {crate_name}"
        );
    }
}

#[test]
fn ui_and_operation_domains_do_not_access_the_filesystem_directly() {
    let crates_root = repository_root().join("crates");
    // The approved disk-backed directory design places one private, owner-only
    // temporary index in the UI crate. It never opens a browsed provider path;
    // all ordinary UI and operation filesystem access still crosses a domain
    // boundary.
    let private_directory_index = crates_root.join("musheen-ui/src/directory/index.rs");
    let mut violations = Vec::new();
    let direct_filesystem_apis = [
        "std::fs",
        "tokio::fs",
        "async_std::fs",
        "async_std :: fs",
        "rustix::fs",
        "rustix :: fs",
        "nix::fcntl",
        "nix :: fcntl",
        "nix::unistd",
        "nix :: unistd",
    ];

    for crate_name in ["musheen-ui", "musheen-ops"] {
        let mut rust_files = Vec::new();
        rust_files_below(&crates_root.join(crate_name).join("src"), &mut rust_files);
        for file in rust_files {
            if file == private_directory_index {
                continue;
            }
            let source = fs::read_to_string(&file).expect("Rust source should be readable");
            // Inline test modules at the end of source files may set up real
            // filesystem fixtures without crossing the production boundary.
            let production_source = source
                .split_once("\n#[cfg(test)]\nmod tests {")
                .map_or(source.as_str(), |(production, _)| production);
            if direct_filesystem_apis
                .iter()
                .any(|api| production_source.contains(api))
            {
                violations.push(file);
            }
        }
    }

    assert!(
        violations.is_empty(),
        "UI and operation domains must use provider or desktop boundaries: {violations:?}"
    );
}

#[test]
fn only_the_ui_crate_owns_native_theme_integration() {
    let crates_root = repository_root().join("crates");
    let mut violations = Vec::new();

    for crate_name in REQUIRED_CRATES {
        if crate_name == "musheen-ui" {
            continue;
        }

        let crate_root = crates_root.join(crate_name);
        let manifest = fs::read_to_string(crate_root.join("Cargo.toml"))
            .expect("crate manifest should be readable");
        if manifest.contains("native-theme") || manifest.contains("native_theme") {
            violations.push(crate_root.join("Cargo.toml"));
        }

        let mut rust_files = Vec::new();
        rust_files_below(&crate_root.join("src"), &mut rust_files);
        for file in rust_files {
            let source = fs::read_to_string(&file).expect("Rust source should be readable");
            if source.contains("native_theme") || source.contains("native-theme") {
                violations.push(file);
            }
        }
    }

    assert!(
        violations.is_empty(),
        "native-theme integration must remain behind the UI boundary: {violations:?}"
    );
}
