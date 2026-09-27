use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

#[test]
fn remote_provider_ci_uses_one_local_container_for_every_protocol() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script_path = root.join("scripts/check-remote-providers.sh");
    let script = fs::read_to_string(&script_path)
        .expect("the local remote-provider check script should exist");
    let dockerfile = fs::read_to_string(root.join("ci/remote-services.Dockerfile"))
        .expect("the combined remote-service Dockerfile should exist");

    assert!(
        fs::metadata(&script_path)
            .expect("the local check script should have metadata")
            .permissions()
            .mode()
            & 0o111
            != 0,
        "the local check script should be executable"
    );
    assert_eq!(
        script.matches("docker run").count(),
        1,
        "the check may start only one container"
    );
    assert!(
        script.contains("cargo test -p musheen-desktop --test remote_live_contract"),
        "the container endpoints should feed the live Rust contract suite"
    );
    assert!(
        script.contains("trap cleanup EXIT"),
        "the disposable service container should always be removed"
    );

    for service in ["apache2", "openssh-server", "vsftpd"] {
        assert!(
            dockerfile.contains(service),
            "the combined image should install {service}"
        );
    }
    for port in ["2121", "2990", "2222", "8080", "8443"] {
        assert!(
            dockerfile.contains(port),
            "the combined image should document service port {port}"
        );
    }
}
