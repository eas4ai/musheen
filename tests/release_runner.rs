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
    assert!(dockerfile.contains("scripts/check-budgets.sh"));
    assert!(dockerfile.contains("scripts/run-benchmarks.sh"));
    assert!(dockerfile.contains("USER musheen"));
    assert!(matrix.contains("1.95.0 stable"));
    assert!(matrix.contains("--no-default-features"));
    assert!(matrix.contains("--all-features"));
    assert!(matrix.contains("--release"));
    assert!(matrix.contains("Cargo.lock changed"));
    assert!(dockerfile.contains("CARGO_INCREMENTAL=0"));
    assert!(dockerfile.contains("CARGO_BUILD_JOBS=8"));
    assert!(dockerfile.contains("CARGO_PROFILE_TEST_DEBUG=0"));
    assert!(dockerfile.contains("verify-msrv.sh"));
    assert!(dockerfile.contains("verify-dependencies.sh"));
    assert!(dockerfile.contains("generate-sbom.py"));
    assert!(dockerfile.contains("cargo fetch --locked"));
}

fn install_fake_benchmarks(fake_bin: &Path) {
    fs::create_dir(fake_bin).unwrap();
    let fake_cargo = fake_bin.join("cargo");
    fs::write(
        &fake_cargo,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$MUSHEEN_CARGO_ARGS\"\nname=directory\nexecutable=$MUSHEEN_FAKE_BENCH\nfor argument in \"$@\"; do\n  if [ \"$argument\" = search ]; then name=search; executable=$MUSHEEN_FAKE_SEARCH_BENCH; fi\ndone\nprintf '{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"%s\"},\"executable\":\"%s\"}\\n' \"$name\" \"$executable\"\n",
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let fake_bench = fake_bin.join("directory-bench");
    fs::write(
        &fake_bench,
        "#!/bin/sh\nprintf '%s' \"$TMPDIR\" > \"$MUSHEEN_BENCH_TMP_RECORD\"\nprintf '%s' \"$*\" > \"$MUSHEEN_BENCH_ARGS\"\nprintf '%s\\n' '{\"case\":\"first_directory_page\",\"items\":512,\"wall_ns\":1,\"cpu_ns\":1,\"peak_rss_kib\":1,\"open_fds\":4,\"queued_pages_max\":1,\"retained_models_max\":512,\"temporary_bytes\":0}' '{\"case\":\"million_item_directory_enumeration\",\"items\":1000000,\"pages\":1954,\"wall_ns\":2,\"cpu_ns\":2,\"peak_rss_kib\":2,\"open_fds_sampled_max\":8,\"queued_pages_max\":1,\"retained_models_max\":4096,\"temporary_bytes_sampled_max\":100}'\nif [ \"${MUSHEEN_FAKE_MISSING:-0}\" = 0 ]; then printf '%s\\n' '{\"case\":\"million_item_directory_scroll\",\"items\":1000000,\"viewports\":101,\"wall_ns\":3,\"cpu_ns\":3,\"peak_rss_kib\":2,\"open_fds\":8,\"queued_pages_max\":0,\"retained_models_max\":4096,\"temporary_bytes\":100}'; fi\n",
    )
    .unwrap();
    fs::set_permissions(&fake_bench, fs::Permissions::from_mode(0o755)).unwrap();
    let fake_search_bench = fake_bin.join("search-bench");
    fs::write(
        &fake_search_bench,
        "#!/bin/sh\nprintf '%s' \"$TMPDIR\" > \"$MUSHEEN_SEARCH_BENCH_TMP_RECORD\"\nprintf '%s' \"$*\" > \"$MUSHEEN_SEARCH_BENCH_ARGS\"\nprintf '%s\\n' '{\"case\":\"search_backpressure_saturation\",\"potential_matches\":1000000,\"queued_matches_max\":2048,\"producer_inflight_matches_max\":256,\"produced_while_stalled\":2304,\"retained_models_max\":0,\"wall_ns\":1,\"cpu_ns\":1,\"peak_rss_kib\":1,\"open_fds\":4,\"temporary_bytes\":0}'\nif [ \"${MUSHEEN_FAKE_SEARCH_MISSING:-0}\" = 0 ]; then printf '%s\\n' '{\"case\":\"million_result_search_refinement\",\"potential_matches\":1000000,\"displayed_matches\":100000,\"produced_matches\":102000,\"accepted_batches\":391,\"queued_matches_max\":2048,\"retained_models_max\":4096,\"state\":\"refine_required\",\"wall_ns\":2,\"cpu_ns\":2,\"peak_rss_kib\":2,\"open_fds\":4,\"temporary_bytes\":0}'; fi\n",
    )
    .unwrap();
    fs::set_permissions(&fake_search_bench, fs::Permissions::from_mode(0o755)).unwrap();
}

fn run_benchmark_fixture(
    root: &Path,
    temporary: &Path,
    fake_bin: &Path,
    missing_directory: bool,
    missing_search: bool,
) -> std::process::Output {
    Command::new(root.join("scripts/run-benchmarks.sh"))
        .env(
            "PATH",
            format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("CARGO_TARGET_DIR", temporary.join("target"))
        .env("MUSHEEN_BENCH_TMP_PARENT", temporary)
        .env("MUSHEEN_FAKE_BENCH", fake_bin.join("directory-bench"))
        .env("MUSHEEN_FAKE_SEARCH_BENCH", fake_bin.join("search-bench"))
        .env(
            "MUSHEEN_FAKE_MISSING",
            if missing_directory { "1" } else { "0" },
        )
        .env(
            "MUSHEEN_FAKE_SEARCH_MISSING",
            if missing_search { "1" } else { "0" },
        )
        .env("MUSHEEN_CARGO_ARGS", temporary.join("cargo-args"))
        .env("MUSHEEN_BENCH_ARGS", temporary.join("bench-args"))
        .env("MUSHEEN_SEARCH_BENCH_ARGS", temporary.join("search-args"))
        .env(
            "MUSHEEN_SEARCH_BENCH_TMP_RECORD",
            temporary.join("search-temp"),
        )
        .env("MUSHEEN_BENCH_TMP_RECORD", temporary.join("bench-temp"))
        .output()
        .unwrap()
}

fn assert_benchmark_fixture_records(temporary: &Path) {
    let cargo_args = fs::read_to_string(temporary.join("cargo-args")).unwrap();
    assert!(cargo_args.contains("--no-run --bench directory"));
    assert!(cargo_args.contains("--no-run --bench search"));
    assert_eq!(
        fs::read_to_string(temporary.join("bench-args")).unwrap(),
        "--bench"
    );
    assert_eq!(
        fs::read_to_string(temporary.join("search-args")).unwrap(),
        "--bench"
    );
    let benchmark_temp = fs::read_to_string(temporary.join("bench-temp")).unwrap();
    let search_temp = fs::read_to_string(temporary.join("search-temp")).unwrap();
    assert!(benchmark_temp.starts_with(temporary.to_str().unwrap()));
    assert_ne!(benchmark_temp, temporary.to_str().unwrap());
    assert!(search_temp.starts_with(temporary.to_str().unwrap()));
    assert_ne!(search_temp, benchmark_temp);
}

#[test]
fn benchmark_runner_isolates_temporary_files_and_rejects_missing_results() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temporary = tempfile::tempdir().unwrap();
    let fake_bin = temporary.path().join("bin");
    install_fake_benchmarks(&fake_bin);
    let complete = run_benchmark_fixture(root, temporary.path(), &fake_bin, false, false);
    assert!(complete.status.success(), "{complete:?}");
    assert_benchmark_fixture_records(temporary.path());
    let missing = run_benchmark_fixture(root, temporary.path(), &fake_bin, true, false);
    assert!(
        !missing.status.success(),
        "missing benchmark case must fail"
    );
    let missing_search = run_benchmark_fixture(root, temporary.path(), &fake_bin, false, true);
    assert!(
        !missing_search.status.success(),
        "missing search benchmark case must fail"
    );
}
