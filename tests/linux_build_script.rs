use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch_directory() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should follow the Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "musheen-linux-build-{}-{nonce}",
        std::process::id()
    ))
}

#[test]
fn linux_build_uses_the_dockerfile_from_its_archived_context() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let scratch = scratch_directory();
    let fake_bin = scratch.join("bin");
    let repository = scratch.join("repository");
    let arguments = scratch.join("docker-arguments");
    let context = scratch.join("docker-context.tar");
    let supplied_docker_config = scratch.join("docker-config");
    let docker_config_record = scratch.join("docker-config-record");
    fs::create_dir_all(&fake_bin).expect("fake executable directory should be created");
    fs::create_dir_all(&supplied_docker_config)
        .expect("supplied Docker configuration directory should be created");
    fs::create_dir_all(repository.join("scripts"))
        .expect("fixture scripts directory should be created");
    fs::create_dir_all(repository.join("ci")).expect("fixture CI directory should be created");

    fs::copy(
        root.join("scripts/check-linux-build.sh"),
        repository.join("scripts/check-linux-build.sh"),
    )
    .expect("Linux build script should be copied into the fixture");
    fs::copy(
        root.join("ci/linux-build.Dockerfile"),
        repository.join("ci/linux-build.Dockerfile"),
    )
    .expect("Dockerfile should be copied into the fixture");

    let docker = fake_bin.join("docker");
    fs::write(
        &docker,
        "#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s\\0' \"$@\" > \"$MUSHEEN_DOCKER_ARGS\"\nprintf '%s' \"${DOCKER_CONFIG:-}\" > \"$MUSHEEN_DOCKER_CONFIG_RECORD\"\ncat > \"$MUSHEEN_DOCKER_CONTEXT\"\n",
    )
    .expect("fake Docker executable should be written");
    let mut permissions = fs::metadata(&docker)
        .expect("fake Docker metadata should be readable")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&docker, permissions).expect("fake Docker should be executable");

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").expect("PATH should be set")
    );
    let initialized = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repository)
        .status()
        .expect("fixture Git repository should initialize");
    assert!(
        initialized.success(),
        "fixture Git repository should initialize"
    );
    let committed = Command::new("git")
        .args([
            "-c",
            "user.name=Musheen Tests",
            "-c",
            "user.email=tests@musheen.invalid",
            "add",
            ".",
        ])
        .current_dir(&repository)
        .status()
        .expect("fixture files should be staged");
    assert!(committed.success(), "fixture files should be staged");
    let committed = Command::new("git")
        .args([
            "-c",
            "user.name=Musheen Tests",
            "-c",
            "user.email=tests@musheen.invalid",
            "commit",
            "--quiet",
            "-m",
            "test fixture",
        ])
        .current_dir(&repository)
        .status()
        .expect("fixture commit should be created");
    assert!(committed.success(), "fixture commit should be created");

    let output = Command::new(repository.join("scripts/check-linux-build.sh"))
        .current_dir(&repository)
        .env("PATH", path)
        .env("DOCKER_CONFIG", &supplied_docker_config)
        .env("MUSHEEN_DOCKER_ARGS", &arguments)
        .env("MUSHEEN_DOCKER_CONFIG_RECORD", &docker_config_record)
        .env("MUSHEEN_DOCKER_CONTEXT", &context)
        .output()
        .expect("Linux build script should start");
    assert!(
        output.status.success(),
        "Linux build script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&docker_config_record).expect("Docker config path should be recorded"),
        supplied_docker_config.to_string_lossy(),
        "Linux build must preserve the caller's Docker credentials"
    );

    let raw_arguments = fs::read(&arguments).expect("Docker arguments should be recorded");
    let arguments: Vec<_> = raw_arguments
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(|argument| String::from_utf8_lossy(argument).into_owned())
        .collect();
    let dockerfile_index = arguments
        .iter()
        .position(|argument| argument == "--file")
        .expect("Docker build should declare its Dockerfile");
    assert_eq!(
        arguments.get(dockerfile_index + 1).map(String::as_str),
        Some("ci/linux-build.Dockerfile"),
        "the Dockerfile path must resolve inside the archived build context"
    );

    let archive = Command::new("tar")
        .args(["-tf"])
        .arg(&context)
        .output()
        .expect("archived build context should be inspectable");
    assert!(
        archive.status.success(),
        "build context should be a valid tar archive"
    );
    assert!(
        String::from_utf8_lossy(&archive.stdout)
            .lines()
            .any(|path| path == "ci/linux-build.Dockerfile"),
        "archived context must contain the selected Dockerfile"
    );

    fs::remove_dir_all(scratch).expect("test scratch directory should be removable");
}

#[test]
fn linux_build_bounds_rust_compiler_resources() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dockerfile = fs::read_to_string(root.join("ci/linux-build.Dockerfile"))
        .expect("Linux build Dockerfile should be readable");

    assert!(
        dockerfile.contains("CARGO_BUILD_JOBS=1"),
        "clean Docker builds must compile one crate job at a time"
    );
    assert!(
        dockerfile.contains("CARGO_INCREMENTAL=0"),
        "clean Docker builds must disable incremental compilation"
    );
    assert!(
        dockerfile.contains("RUST_TEST_THREADS=1"),
        "clean builds must bound native UI test concurrency"
    );
    assert!(
        dockerfile.contains("RUST_MIN_STACK=16777216"),
        "clean Docker builds must give rustc enough worker stack"
    );
    assert!(
        dockerfile.contains("--mount=type=cache,target=/usr/local/cargo/registry"),
        "clean Docker builds must reuse the Cargo registry cache"
    );
    assert!(
        dockerfile.contains("--mount=type=cache,target=/workspace/target"),
        "clean Docker builds must reuse compiled artifacts"
    );
}

#[test]
fn linux_build_installs_native_and_package_validation_dependencies() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dockerfile = fs::read_to_string(root.join("ci/linux-build.Dockerfile"))
        .expect("Linux build Dockerfile should be readable");

    assert!(
        dockerfile.contains(
            "MUSHEEN_ARCHIVE_WORKER_BINARY=/workspace/target/debug/musheen-archive-worker"
        )
    );
    assert!(dockerfile.contains(
        "MUSHEEN_THUMBNAIL_WORKER_BINARY=/workspace/target/debug/musheen-thumbnail-worker"
    ));

    for package in [
        "appstream",
        "desktop-file-utils",
        "file",
        "jq",
        "libacl1-dev",
        "libfontconfig1-dev",
        "libfreetype6-dev",
        "libsmbclient-dev",
        "libxcb1-dev",
        "libxkbcommon-dev",
        "libxkbcommon-x11-dev",
        "librsvg2-bin",
        "pkg-config",
        "python3",
    ] {
        assert!(
            dockerfile.contains(package),
            "clean Docker builds must install native or package validation dependency {package}"
        );
    }
}
