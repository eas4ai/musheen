use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn fixture(root: &Path, rust_version: &str) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn ready() -> bool { true }\n").unwrap();
    fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname = \"musheen\"\nversion = \"0.1.0\"\nedition = \"2024\"\nrust-version = \"{rust_version}\"\nlicense = \"GPL-3.0-or-later\"\n"
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
fn sbom_and_license_notice_cover_the_locked_graph_without_build_paths() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/generate-sbom.py");
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("repository");
    fixture(&repository, "1.95");
    let sample = repository.join("vendor/sample");
    fs::create_dir_all(sample.join("src")).unwrap();
    fs::write(
        sample.join("src/lib.rs"),
        "pub fn ready() -> bool { true }\n",
    )
    .unwrap();
    fs::write(
        sample.join("Cargo.toml"),
        "[package]\nname = \"sample\"\nversion = \"0.2.0\"\nedition = \"2024\"\nlicense = \"MIT\"\n",
    )
    .unwrap();
    let mut manifest = fs::read_to_string(repository.join("Cargo.toml")).unwrap();
    manifest.push_str("\n[dependencies]\nsample = { path = \"vendor/sample\" }\n");
    fs::write(repository.join("Cargo.toml"), manifest).unwrap();
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(&repository)
        .output()
        .unwrap();
    assert!(lock.status.success(), "{lock:?}");

    let artifact_dir = temporary.path().join("artifacts");
    let output = Command::new("python3")
        .arg(&script)
        .arg(&repository)
        .arg(&artifact_dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let raw = fs::read_to_string(artifact_dir.join("musheen.cdx.json")).unwrap();
    let bom: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(bom["bomFormat"], "CycloneDX");
    assert_eq!(bom["specVersion"], "1.6");
    assert_eq!(bom["metadata"]["component"]["name"], "musheen");
    assert_eq!(bom["components"][0]["name"], "sample");
    assert_eq!(bom["components"][0]["licenses"][0]["expression"], "MIT");
    assert!(
        bom["dependencies"][0]["dependsOn"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "pkg:cargo/sample@0.2.0")
    );
    let notice = fs::read_to_string(artifact_dir.join("THIRD_PARTY_LICENSES.md")).unwrap();
    assert!(notice.contains("sample | 0.2.0 | MIT"));
    assert!(!raw.contains(&repository.to_string_lossy().to_string()));
    assert!(!notice.contains(&repository.to_string_lossy().to_string()));
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

#[test]
fn budget_checker_rejects_a_cargo_run_with_no_tests() {
    let (output, _temporary) = run_budget_checker_with_fake_cargo(
        "printf 'test result: ok. 0 passed; 0 failed; 0 ignored\\n'",
    );
    assert!(!output.status.success(), "empty test filter must fail");
    assert!(String::from_utf8_lossy(&output.stderr).contains("no budget test ran"));
}

#[test]
fn budget_checker_runs_a_case_for_every_resource_limit() {
    let (output, temporary) = run_budget_checker_with_fake_cargo(
        "printf '%s\\n' \"$*\" >> \"$MUSHEEN_BUDGET_CALL_LOG\"\nprintf 'test result: ok. 1 passed; 0 failed; 0 ignored\\n'",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fs::read_to_string(temporary.path().join("calls")).unwrap();
    for case in [
        "operations_capture_an_immutable_resource_limit_snapshot",
        "streaming_directory_model_pages_through_one_million_items",
        "million_result_producer_requests_refinement_without_unbounded_models",
        "preview_limits_reject_values_above_the_read_and_retention_budgets",
        "thumbnail_limits_reject_work_above_the_documented_budget",
        "default_archive_staging_budget_does_not_exceed_ten_gib",
        "scrollback_drops_oldest_complete_lines_at_both_limits",
        "configured_pool_rejects_values_above_the_documented_budgets",
        "defaults_provider_limits_and_fifo_progress_are_enforced",
    ] {
        assert!(calls.contains(case), "budget case not run: {case}");
    }
    assert!(
        calls.contains("--ignored"),
        "the million-item case must run"
    );
}

fn run_budget_checker_with_fake_cargo(body: &str) -> (Output, tempfile::TempDir) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-budgets.sh");
    let temporary = tempfile::tempdir().unwrap();
    let fake_cargo = temporary.path().join("cargo");
    fs::write(&fake_cargo, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        temporary.path().display(),
        std::env::var("PATH").unwrap()
    );

    let output = Command::new(&script)
        .env("PATH", path)
        .env("MUSHEEN_BUDGET_CALL_LOG", temporary.path().join("calls"))
        .env(
            "CARGO_TARGET_DIR",
            std::env::var("CARGO_TARGET_DIR")
                .unwrap_or_else(|_| temporary.path().join("target").display().to_string()),
        )
        .output()
        .unwrap();
    (output, temporary)
}
