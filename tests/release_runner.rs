use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

#[test]
fn local_release_runner_builds_the_committed_tree_in_one_docker_build() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("repository");
    let fake_bin = temporary.path().join("bin");
    let scratch_base = temporary.path().join("scratchpads");
    let runtime_dir = temporary.path().join("runtime");
    let docker_config = temporary.path().join("docker-config");
    fs::create_dir_all(repository.join("scripts")).unwrap();
    fs::create_dir_all(repository.join("ci")).unwrap();
    for directory in [&fake_bin, &scratch_base, &runtime_dir, &docker_config] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::copy(
        root.join("scripts/run-release-matrix.sh"),
        repository.join("scripts/run-release-matrix.sh"),
    )
    .unwrap();
    fs::copy(
        root.join("ci/release.Dockerfile"),
        repository.join("ci/release.Dockerfile"),
    )
    .unwrap();

    let docker = fake_bin.join("docker");
    fs::write(
        &docker,
        "#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s\\0' \"$@\" > \"$MUSHEEN_DOCKER_ARGS\"\nprintf '%s' \"${DOCKER_CONFIG:-}\" > \"$MUSHEEN_DOCKER_CONFIG_RECORD\"\ncat > \"$MUSHEEN_DOCKER_CONTEXT\"\nif [[ ${MUSHEEN_FAKE_OMIT_ARTIFACTS:-0} == 1 ]]; then exit 0; fi\nprevious=\nfor argument in \"$@\"; do\n  if [[ $previous == --output ]]; then\n    destination=${argument#type=local,dest=}\n    printf '{}\\n' > \"$destination/musheen.cdx.json\"\n    printf 'notice\\n' > \"$destination/THIRD_PARTY_LICENSES.md\"\n    break\n  fi\n  previous=$argument\ndone\n",
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["add", "."])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-c",
                "user.name=Musheen Tests",
                "-c",
                "user.email=tests@musheen.invalid",
                "commit",
                "--quiet",
                "-m",
                "release fixture",
            ])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success()
    );

    let arguments = temporary.path().join("docker-arguments");
    let context = temporary.path().join("docker-context.tar");
    let config_record = temporary.path().join("docker-config-record");
    let output = Command::new(repository.join("scripts/run-release-matrix.sh"))
        .current_dir(&repository)
        .env(
            "PATH",
            format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("DOCKER_CONFIG", &docker_config)
        .env("MUSHEEN_SCRATCH_BASE", &scratch_base)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("MUSHEEN_DOCKER_ARGS", &arguments)
        .env("MUSHEEN_DOCKER_CONTEXT", &context)
        .env("MUSHEEN_DOCKER_CONFIG_RECORD", &config_record)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(&config_record).unwrap(),
        docker_config.to_string_lossy()
    );
    let args = fs::read(&arguments).unwrap();
    let args: Vec<_> = args
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    assert_eq!(args.first().map(String::as_str), Some("build"));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--file", "ci/release.Dockerfile"])
    );
    assert!(
        args.windows(2)
            .any(|pair| { pair[0] == "--output" && pair[1].starts_with("type=local,dest=") })
    );
    assert_eq!(args.last().map(String::as_str), Some("-"));
    let archive = Command::new("tar")
        .arg("-tf")
        .arg(&context)
        .output()
        .unwrap();
    assert!(archive.status.success());
    assert!(
        String::from_utf8_lossy(&archive.stdout)
            .lines()
            .any(|path| path == "ci/release.Dockerfile")
    );

    let missing_artifacts = Command::new(repository.join("scripts/run-release-matrix.sh"))
        .current_dir(&repository)
        .env(
            "PATH",
            format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("MUSHEEN_SCRATCH_BASE", &scratch_base)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("MUSHEEN_DOCKER_ARGS", &arguments)
        .env("MUSHEEN_DOCKER_CONTEXT", &context)
        .env("MUSHEEN_DOCKER_CONFIG_RECORD", &config_record)
        .env("MUSHEEN_FAKE_OMIT_ARTIFACTS", "1")
        .output()
        .unwrap();
    assert!(
        !missing_artifacts.status.success(),
        "the release runner must reject a successful Docker build without exported evidence"
    );
}

#[test]
fn release_workflow_has_no_development_triggers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow = fs::read_to_string(root.join(".github/workflows/release.yml")).unwrap();
    assert!(workflow.contains("workflow_dispatch:"));
    assert!(workflow.contains("tags:"));
    assert!(workflow.contains("scripts/run-release-matrix.sh"));
    assert!(workflow.contains("if-no-files-found: error"));
    assert!(!workflow.contains("pull_request:"));
    assert!(!workflow.contains("branches:"));
}

#[test]
fn release_container_tests_both_toolchains_features_and_profiles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dockerfile = fs::read_to_string(root.join("ci/release.Dockerfile")).unwrap();
    let matrix = fs::read_to_string(root.join("scripts/verify-release-matrix.sh")).unwrap();
    assert!(dockerfile.contains("verify-release-matrix.sh"));
    assert!(matrix.contains("1.95.0 stable"));
    assert!(matrix.contains("--no-default-features"));
    assert!(matrix.contains("--all-features"));
    assert!(matrix.contains("--release"));
    assert!(matrix.contains("Cargo.lock changed"));
    assert!(dockerfile.contains("CARGO_INCREMENTAL=0"));
    assert!(dockerfile.contains("CARGO_BUILD_JOBS=8"));
    assert!(dockerfile.contains("verify-msrv.sh"));
    assert!(dockerfile.contains("verify-dependencies.sh"));
    assert!(dockerfile.contains("generate-sbom.py"));
}
