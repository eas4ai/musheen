use std::io::Write as _;
use std::process::{Command, Stdio};

use musheen_desktop::privilege::{
    BROKER_PROTOCOL_ARGUMENT, BROKER_PROTOCOL_FRAME, BROKER_PROTOCOL_VERSION, BrokerLaunch,
    BrokerRequest, PrivilegeProvider, encode_broker_request,
};

/// The line the broker writes before it reads its request (SYS-034).
fn protocol_line() -> Vec<u8> {
    format!("{BROKER_PROTOCOL_FRAME}{BROKER_PROTOCOL_VERSION}\n").into_bytes()
}

fn broker_arguments(request: &BrokerRequest) -> Vec<std::ffi::OsString> {
    BrokerLaunch::new("/usr/lib/musheen/musheen-broker", PrivilegeProvider::Polkit)
        .arguments_for(request)
        .into_iter()
        .skip(2)
        .collect()
}

#[test]
fn broker_binary_rejects_malformed_or_untyped_input_before_authorization() {
    let target = tempfile::tempdir().unwrap();
    let request = BrokerRequest::open_directory(target.path()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_musheen-broker"))
        .args(broker_arguments(&request))
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"operation\":{\"kind\":\"shell\",\"command\":\"id\"}}\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(!output.status.success());
    assert_eq!(output.stdout, protocol_line(), "only the protocol line");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.trim(), "invalid broker request");
    assert!(!stderr.contains("shell"));
    assert!(!stderr.contains("rm -rf"));
}

#[test]
fn broker_binary_refuses_a_valid_request_when_not_elevated() {
    assert_ne!(rustix::process::geteuid().as_raw(), 0);
    let target = tempfile::tempdir().unwrap();
    let request = BrokerRequest::open_directory(target.path()).unwrap();
    let frame = encode_broker_request(&request).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_musheen-broker"))
        .args(broker_arguments(&request))
        .env_clear()
        .env(
            "PKEXEC_UID",
            rustix::process::geteuid().as_raw().to_string(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{frame}").unwrap();

    let output = child.wait_with_output().unwrap();

    assert!(!output.status.success());
    assert_eq!(output.stdout, protocol_line(), "only the protocol line");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "broker must run elevated"
    );
}

#[test]
fn broker_binary_rejects_a_target_that_differs_from_the_approved_argv() {
    let target = tempfile::tempdir().unwrap();
    let request = BrokerRequest::open_directory(target.path()).unwrap();
    let frame = encode_broker_request(&request).unwrap();
    let mut arguments = broker_arguments(&request);
    *arguments.last_mut().unwrap() = "/different/target".into();
    let mut child = Command::new(env!("CARGO_BIN_EXE_musheen-broker"))
        .args(arguments)
        .env_clear()
        .env(
            "PKEXEC_UID",
            rustix::process::geteuid().as_raw().to_string(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{frame}").unwrap();

    let output = child.wait_with_output().unwrap();

    assert!(!output.status.success());
    assert_eq!(output.stdout, protocol_line(), "only the protocol line");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "request binding mismatch"
    );
}

#[test]
fn broker_binary_refuses_a_listing_that_does_not_follow_its_open_request() {
    let target = tempfile::tempdir().unwrap();
    let root = musheen_desktop::privilege::ElevatedRootReference::capture(target.path()).unwrap();
    let request = BrokerRequest::read_directory(root, std::path::PathBuf::new()).unwrap();
    let frame = encode_broker_request(&request).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_musheen-broker"))
        .args(broker_arguments(&request))
        .env_clear()
        .env(
            "PKEXEC_UID",
            rustix::process::geteuid().as_raw().to_string(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{frame}").unwrap();

    let output = child.wait_with_output().unwrap();

    // A listing runs only inside the session its Open as Administrator
    // request authorized (SYS-034), even when its arguments match.
    assert!(!output.status.success());
    assert_eq!(output.stdout, protocol_line(), "only the protocol line");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "request binding mismatch"
    );
}

#[test]
fn broker_binary_names_its_protocol_version_without_privileges() {
    let output = Command::new(env!("CARGO_BIN_EXE_musheen-broker"))
        .arg(BROKER_PROTOCOL_ARGUMENT)
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, protocol_line());
}
