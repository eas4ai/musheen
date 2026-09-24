use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::Command;

use musheen_desktop::privilege::{ADMIN_ACTION_IDS, INSTALLED_BROKER_PATH};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn polkit_installer_stages_fixed_paths_and_modes() {
    let root = repository_root();
    let stage = std::env::temp_dir().join(format!(
        "musheen-polkit-install-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir(&stage).unwrap();
    let broker_fixture = stage.join("broker-fixture");
    fs::write(&broker_fixture, b"fixture").unwrap();

    let status = Command::new("/bin/sh")
        .arg(root.join("packaging/install-polkit-policy.sh"))
        .env("DESTDIR", &stage)
        .env("MUSHEEN_BROKER_BINARY", &broker_fixture)
        .status()
        .unwrap();

    assert!(status.success());
    let broker = stage.join("usr/lib/musheen/musheen-broker");
    let policy = stage.join("usr/share/polkit-1/actions/org.musheen.Musheen.policy");
    assert_eq!(
        fs::metadata(broker).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(policy).unwrap().permissions().mode() & 0o777,
        0o644
    );
    fs::remove_dir_all(stage).unwrap();
}

fn action_block<'a>(policy: &'a str, action_id: &str) -> &'a str {
    let start = format!("<action id=\"{action_id}\">");
    policy
        .split_once(&start)
        .and_then(|(_, suffix)| suffix.split_once("</action>"))
        .map(|(block, _)| block)
        .unwrap_or_else(|| panic!("policy must register {action_id}"))
}

fn assert_well_formed_policy_document(policy: &str) {
    assert!(policy.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
    assert!(policy.contains("<!DOCTYPE policyconfig"));
    assert_eq!(policy.matches("<policyconfig>").count(), 1);
    assert_eq!(policy.matches("</policyconfig>").count(), 1);
    assert!(policy.trim_end().ends_with("</policyconfig>"));
    for element in ["action", "description", "message", "defaults", "annotate"] {
        assert_eq!(
            policy.matches(&format!("<{element}")).count(),
            policy.matches(&format!("</{element}>")).count(),
            "unbalanced {element} elements"
        );
    }
}

#[test]
fn installed_polkit_policy_covers_every_exact_broker_action() {
    let root = repository_root();
    let policy_path = root.join("packaging/polkit/org.musheen.Musheen.policy");
    let policy = fs::read_to_string(&policy_path).expect("Musheen must ship its polkit policy");
    assert_well_formed_policy_document(&policy);
    assert_eq!(
        policy.matches("<action id=").count(),
        ADMIN_ACTION_IDS.len()
    );
    assert!(!policy.contains("auth_admin_keep"));

    for action_id in ADMIN_ACTION_IDS {
        let block = action_block(&policy, action_id);
        assert!(block.contains("$(command_line)"));
        assert_eq!(block.matches("<allow_any>no</allow_any>").count(), 1);
        assert_eq!(
            block.matches("<allow_inactive>no</allow_inactive>").count(),
            1
        );
        assert_eq!(
            block
                .matches("<allow_active>auth_admin</allow_active>")
                .count(),
            1
        );
        assert!(block.contains(&format!(
            "<annotate key=\"org.freedesktop.policykit.exec.path\">{INSTALLED_BROKER_PATH}</annotate>"
        )));
        assert!(block.contains(&format!(
            "<annotate key=\"org.freedesktop.policykit.exec.argv1\">--action-id={action_id}</annotate>"
        )));
    }

    let installer = fs::read_to_string(root.join("packaging/install-polkit-policy.sh"))
        .expect("the policy must have a package installation entrypoint");
    assert!(installer.contains("/usr/share/polkit-1/actions/org.musheen.Musheen.policy"));
    assert!(installer.contains("/usr/lib/musheen/musheen-broker"));
    assert!(installer.contains("DESTDIR"));
    assert!(installer.contains("-m 0644"));
    assert!(installer.contains("-m 0755"));

    let production_broker =
        fs::read_to_string(root.join("crates/musheen-desktop/src/bin/musheen-broker.rs")).unwrap();
    let privilege_module =
        fs::read_to_string(root.join("crates/musheen-desktop/src/privilege/mod.rs")).unwrap();
    assert!(!production_broker.contains("PolkitAuthorizer"));
    assert!(!privilege_module.contains("mod polkit"));
}
