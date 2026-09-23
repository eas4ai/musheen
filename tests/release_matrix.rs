use std::fs;
use std::path::Path;
use std::process::Command;

fn fixture(root: &Path, rust_version: &str) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn ready() -> bool { true }\n").unwrap();
    fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname = \"musheen\"\nversion = \"0.1.0\"\nedition = \"2024\"\nrust-version = \"{rust_version}\"\n"
        ),
    )
    .unwrap();
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.95.0\"\n",
    )
    .unwrap();
    let output = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn msrv_verifier_accepts_matching_packages_and_rejects_mismatches() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/verify-msrv.sh");
    let temporary = tempfile::tempdir().unwrap();
    let matching = temporary.path().join("matching");
    let mismatch = temporary.path().join("mismatch");
    fixture(&matching, "1.95");
    fixture(&mismatch, "1.94");

    let accepted = Command::new(&script).arg(&matching).output().unwrap();
    assert!(
        accepted.status.success(),
        "matching MSRV must pass: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let rejected = Command::new(&script).arg(&mismatch).output().unwrap();
    assert!(
        !rejected.status.success(),
        "a package below the supported MSRV must fail"
    );
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("rust-version"));
}

#[test]
fn dependency_policy_rejects_expired_advisory_exceptions_and_duplicate_role_crates() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/verify-dependency-policy.py");
    let temporary = tempfile::tempdir().unwrap();
    let fixture_root = temporary.path().join("repository");
    fixture(&fixture_root, "1.95");
    let policy = fixture_root.join("deny.toml");
    let valid_reason =
        "A transitive unmaintained dependency has no safe replacement. Expires 2999-01-01.";
    fs::write(
        &policy,
        format!(
            "[advisories]\nignore = [{{ id = \"RUSTSEC-2024-0001\", reason = \"{valid_reason}\" }}]\n"
        ),
    )
    .unwrap();
    let accepted = Command::new("python3")
        .arg(&script)
        .arg(&fixture_root)
        .output()
        .unwrap();
    assert!(
        accepted.status.success(),
        "dated exception must pass: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    fs::write(
        &policy,
        "[advisories]\nignore = [{ id = \"RUSTSEC-2024-0001\", reason = \"Unmaintained and ignored. Expires 2020-01-01.\" }]\n",
    )
    .unwrap();
    let expired = Command::new("python3")
        .arg(&script)
        .arg(&fixture_root)
        .output()
        .unwrap();
    assert!(!expired.status.success(), "expired exceptions must fail");

    fs::write(
        &policy,
        format!(
            "[advisories]\nignore = [{{ id = \"RUSTSEC-2024-0001\", reason = \"{valid_reason}\" }}]\n"
        ),
    )
    .unwrap();
    let mut lockfile = fs::read_to_string(fixture_root.join("Cargo.lock")).unwrap();
    lockfile.push_str(
        "\n[[package]]\nname = \"gpui-kit\"\nversion = \"0.6.4\"\n\
         \n[[package]]\nname = \"gpui-kit\"\nversion = \"0.6.5\"\n",
    );
    fs::write(fixture_root.join("Cargo.lock"), lockfile).unwrap();
    let duplicate = Command::new("python3")
        .arg(&script)
        .arg(&fixture_root)
        .output()
        .unwrap();
    assert!(
        !duplicate.status.success(),
        "duplicate GPUI Kit versions must fail"
    );
}
