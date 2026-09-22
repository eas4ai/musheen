use std::io::Write as _;
use std::process::{Command, Stdio};

#[test]
fn broker_binary_rejects_malformed_or_untyped_input_before_authorization() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_musheen-broker"))
        .args(["--stdio", "--provider=polkit"])
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
