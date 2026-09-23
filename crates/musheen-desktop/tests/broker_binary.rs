use std::io::Write as _;
use std::process::{Command, Stdio};

use musheen_desktop::privilege::{
    BrokerLaunch, BrokerRequest, PrivilegeProvider, encode_broker_request,
};

fn broker_arguments(request: &BrokerRequest) -> Vec<std::ffi::OsString> {
    BrokerLaunch::new("/usr/libexec/musheen-broker", PrivilegeProvider::Polkit)
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
    assert!(output.stdout.is_empty());
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
    assert!(output.stdout.is_empty());
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
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "request binding mismatch"
    );
}
