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
    let arguments = scratch.join("docker-arguments");
    let context = scratch.join("docker-context.tar");
    fs::create_dir_all(&fake_bin).expect("fake executable directory should be created");

    let docker = fake_bin.join("docker");
    fs::write(
        &docker,
        "#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s\\0' \"$@\" > \"$MUSHEEN_DOCKER_ARGS\"\ncat > \"$MUSHEEN_DOCKER_CONTEXT\"\n",
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
    let output = Command::new(root.join("scripts/check-linux-build.sh"))
        .current_dir(&root)
        .env("PATH", path)
        .env("MUSHEEN_DOCKER_ARGS", &arguments)
        .env("MUSHEEN_DOCKER_CONTEXT", &context)
        .output()
        .expect("Linux build script should start");
    assert!(
        output.status.success(),
        "Linux build script failed: {}",
        String::from_utf8_lossy(&output.stderr)
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
