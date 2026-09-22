use musheen_core::CancellationToken;
use musheen_desktop::SecretBuffer;
use musheen_desktop::privilege::{
    AuditOutcome, AuditPhase, AuditRecord, AuditSink, AuthorizationError, AuthorizationGrant,
    AuthorizationRequest, Authorizer, Broker, BrokerError, BrokerLaunch, BrokerOperation,
    BrokerOutput, BrokerRequest, BrokerResponse, BrokerTransport, Clock, JsonAuditLog,
    OperationRunner, PrivilegeProvider, ProcessBrokerTransport, RootCapabilityDescriptor,
    RootGrant, RootedStore, SudoPtyBrokerTransport, SystemOperationRunner, ValidatedRequest,
    encode_broker_response,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
struct FixedClock(Arc<Mutex<u64>>);

impl FixedClock {
    fn new(now: u64) -> Self {
        Self(Arc::new(Mutex::new(now)))
    }

    fn set(&self, now: u64) {
        *self.0.lock().unwrap() = now;
    }
}

impl Clock for FixedClock {
    fn now_unix_millis(&self) -> u64 {
        *self.0.lock().unwrap()
    }
}

#[derive(Clone)]
struct FakeAuthorizer {
    result: Arc<Mutex<Result<AuthorizationGrant, AuthorizationError>>>,
    observed: Arc<Mutex<Vec<AuthorizationRequest>>>,
    after_authorization: AuthorizationHook,
}

type AuthorizationHook = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

impl FakeAuthorizer {
    fn granting(expires_at_unix_millis: u64) -> Self {
        Self {
            result: Arc::new(Mutex::new(Ok(AuthorizationGrant::new(
                "test-subject",
                expires_at_unix_millis,
            )))),
            observed: Arc::default(),
            after_authorization: Arc::default(),
        }
    }

    fn denying() -> Self {
        let authorizer = Self::granting(0);
        *authorizer.result.lock().unwrap() = Err(AuthorizationError::Denied);
        authorizer
    }

    fn after_authorization(&self, callback: impl FnOnce() + Send + 'static) {
        *self.after_authorization.lock().unwrap() = Some(Box::new(callback));
    }
}

impl Authorizer for FakeAuthorizer {
    fn authorize(
        &self,
        request: &AuthorizationRequest,
    ) -> Result<AuthorizationGrant, AuthorizationError> {
        self.observed.lock().unwrap().push(request.clone());
        let result = self.result.lock().unwrap().clone();
        if let Some(callback) = self.after_authorization.lock().unwrap().take() {
            callback();
        }
        result
    }
}

#[derive(Clone, Default)]
struct RecordingRunner {
    requests: Arc<Mutex<Vec<RecordedExecution>>>,
    crash: Arc<Mutex<bool>>,
}

#[derive(Clone, Debug)]
struct RecordedExecution {
    operation: BrokerOperation,
    environment: BTreeMap<String, String>,
}

impl OperationRunner for RecordingRunner {
    fn execute(&self, request: ValidatedRequest) -> Result<BrokerOutput, BrokerError> {
        if *self.crash.lock().unwrap() {
            return Err(BrokerError::BrokerCrashed);
        }
        self.requests.lock().unwrap().push(RecordedExecution {
            operation: request.operation().clone(),
            environment: request.environment().clone(),
        });
        Ok(match request.operation() {
            BrokerOperation::OpenDirectory { target } => BrokerOutput::DirectoryGranted(
                RootCapabilityDescriptor::capture(target, u64::MAX).unwrap(),
            ),
            BrokerOperation::RunExecutable { .. } => BrokerOutput::Exited(0),
            BrokerOperation::ReadDirectory { .. } => BrokerOutput::DirectoryEntries(Vec::new()),
        })
    }
}

#[derive(Clone, Default)]
struct RecordingAudit(Arc<Mutex<Vec<AuditRecord>>>);

impl AuditSink for RecordingAudit {
    fn record(&self, record: &AuditRecord) -> Result<(), BrokerError> {
        self.0.lock().unwrap().push(record.clone());
        Ok(())
    }
}

fn broker<S: AuditSink>(
    authorizer: FakeAuthorizer,
    runner: RecordingRunner,
    audit: S,
    clock: FixedClock,
) -> Broker<FakeAuthorizer, RecordingRunner, S, FixedClock> {
    Broker::new(authorizer, runner, audit, clock)
}

#[test]
fn authorization_denial_and_expiry_refuse_before_execution() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("protected");
    fs::create_dir(&target).unwrap();
    let runner = RecordingRunner::default();
    let audit = RecordingAudit::default();
    let clock = FixedClock::new(100);

    let denied = broker(
        FakeAuthorizer::denying(),
        runner.clone(),
        audit.clone(),
        clock.clone(),
    )
    .handle(BrokerRequest::open_directory(&target).unwrap());
    assert_eq!(denied, Err(BrokerError::AuthorizationDenied));

    let expired = broker(
        FakeAuthorizer::granting(99),
        runner.clone(),
        audit.clone(),
        clock,
    )
    .handle(BrokerRequest::open_directory(&target).unwrap());
    assert_eq!(expired, Err(BrokerError::AuthorizationExpired));
    assert!(runner.requests.lock().unwrap().is_empty());
    assert_eq!(audit.0.lock().unwrap().len(), 4);
}

#[test]
fn target_replacement_after_approval_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("protected");
    let moved = root.path().join("original");
    fs::create_dir(&target).unwrap();
    let authorizer = FakeAuthorizer::granting(1_000);
    let swap_target = target.clone();
    authorizer.after_authorization(move || {
        fs::rename(&swap_target, &moved).unwrap();
        fs::create_dir(&swap_target).unwrap();
    });
    let runner = RecordingRunner::default();
    let result = broker(
        authorizer,
        runner.clone(),
        RecordingAudit::default(),
        FixedClock::new(100),
    )
    .handle(BrokerRequest::open_directory(&target).unwrap());

    assert_eq!(result, Err(BrokerError::TargetReplaced));
    assert!(runner.requests.lock().unwrap().is_empty());
}

#[test]
fn rooted_store_rejects_parent_absolute_and_symlink_escape() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("child")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
    let grant = RootGrant::open(root.path(), "grant-1", 1_000, PrivilegeProvider::Polkit).unwrap();
    let store = RootedStore::new(grant, FixedClock::new(100));

    assert!(store.resolve(Path::new("child")).is_ok());
    assert_eq!(
        store.resolve(Path::new("../outside")),
        Err(BrokerError::ScopeEscape)
    );
    assert_eq!(store.resolve(outside.path()), Err(BrokerError::ScopeEscape));
    assert_eq!(
        store.resolve(Path::new("escape")),
        Err(BrokerError::SymlinkRefused)
    );
}

#[test]
fn run_request_preserves_hostile_arguments_without_shell_and_scrubs_environment() {
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("runner;touch-pwned");
    fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
    fs::set_permissions(&executable, permissions).unwrap();
    let runner = RecordingRunner::default();
    let request = BrokerRequest::run_executable(
        &executable,
        ["one;touch injected", "$(touch injected-2)", "line\nvalue"],
    )
    .unwrap();
    let result = broker(
        FakeAuthorizer::granting(1_000),
        runner.clone(),
        RecordingAudit::default(),
        FixedClock::new(100),
    )
    .handle_with_environment(
        request,
        BTreeMap::from([
            ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
            ("LD_PRELOAD".to_owned(), "/tmp/evil.so".to_owned()),
            ("DOCKER_AUTH_TOKEN".to_owned(), "must-not-cross".to_owned()),
            ("HOME".to_owned(), "/root".to_owned()),
        ]),
    );

    assert_eq!(result, Ok(BrokerOutput::Exited(0)));
    let recorded = runner.requests.lock().unwrap();
    let BrokerOperation::RunExecutable { arguments, .. } = &recorded[0].operation else {
        panic!("the typed run operation must reach the runner");
    };
    assert_eq!(
        arguments.as_ref(),
        ["one;touch injected", "$(touch injected-2)", "line\nvalue"]
    );
    assert_eq!(
        recorded[0].environment,
        BTreeMap::from([("LANG".to_owned(), "en_US.UTF-8".to_owned())])
    );
}

#[test]
fn request_schema_is_narrow_and_rejects_untrusted_shapes() {
    assert!(BrokerRequest::open_directory("relative/path").is_err());
    assert!(BrokerRequest::run_executable("relative", ["arg"]).is_err());
    assert!(BrokerRequest::run_executable("/bin/true", std::iter::repeat_n("x", 257)).is_err());
    assert!(
        serde_json::from_str::<BrokerRequest>(
            r#"{"id":"x","operation":{"kind":"shell","command":"rm -rf /"}}"#
        )
        .is_err()
    );
}

#[test]
fn audit_records_operation_target_provider_and_outcome_without_environment_or_arguments() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("protected");
    fs::create_dir(&target).unwrap();
    let audit = RecordingAudit::default();
    broker(
        FakeAuthorizer::granting(1_000),
        RecordingRunner::default(),
        audit.clone(),
        FixedClock::new(100),
    )
    .handle(BrokerRequest::open_directory(&target).unwrap())
    .unwrap();

    let records = audit.0.lock().unwrap();
    assert_eq!(records.len(), 4);
    assert_eq!(records[0].target(), target);
    assert_eq!(records[0].provider(), PrivilegeProvider::Polkit);
    assert_eq!(records[0].phase(), AuditPhase::Attempt);
    assert_eq!(records[0].outcome(), AuditOutcome::Started);
    assert_eq!(records[1].phase(), AuditPhase::Authorization);
    assert_eq!(records[2].phase(), AuditPhase::Dispatch);
    assert_eq!(records[3].phase(), AuditPhase::Completion);
    assert_eq!(records[3].outcome(), AuditOutcome::Succeeded);
    let serialized = serde_json::to_string(&records[0]).unwrap();
    assert!(!serialized.contains("DOCKER_AUTH_TOKEN"));
    assert!(!serialized.contains("arguments"));
}

#[test]
fn broker_crash_is_typed_and_audited() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("protected");
    fs::create_dir(&target).unwrap();
    let runner = RecordingRunner::default();
    *runner.crash.lock().unwrap() = true;
    let audit = RecordingAudit::default();
    let result = broker(
        FakeAuthorizer::granting(1_000),
        runner,
        audit.clone(),
        FixedClock::new(100),
    )
    .handle(BrokerRequest::open_directory(&target).unwrap());

    assert_eq!(result, Err(BrokerError::BrokerCrashed));
    let records = audit.0.lock().unwrap();
    assert_eq!(records.len(), 4);
    assert_eq!(records[2].phase(), AuditPhase::Dispatch);
    assert_eq!(records[2].outcome(), AuditOutcome::Started);
    assert_eq!(records[3].phase(), AuditPhase::Completion);
    assert_eq!(records[3].outcome(), AuditOutcome::Failed);
}

#[test]
fn fake_authorizer_runs_without_privilege_and_confirmation_names_provider_command_and_target() {
    assert_ne!(
        rustix::process::geteuid().as_raw(),
        0,
        "test must run unprivileged"
    );
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("tool");
    fs::write(&executable, b"tool").unwrap();
    let request = BrokerRequest::run_executable(&executable, ["--safe"]).unwrap();
    let summary = request.confirmation(PrivilegeProvider::Sudo);

    assert_eq!(summary.provider(), "sudo");
    assert_eq!(summary.command(), "Run as Administrator");
    assert_eq!(summary.target(), executable);
    assert_eq!(summary.arguments(), &["--safe"]);
}

#[test]
fn expired_root_grant_cannot_browse() {
    let root = tempfile::tempdir().unwrap();
    let clock = FixedClock::new(100);
    let store = RootedStore::new(
        RootGrant::open(root.path(), "grant-1", 101, PrivilegeProvider::Polkit).unwrap(),
        clock.clone(),
    );
    clock.set(102);
    assert_eq!(
        store.resolve(Path::new(".")),
        Err(BrokerError::AuthorizationExpired)
    );
}

#[test]
fn request_paths_remain_lossless() {
    use std::os::unix::ffi::OsStringExt as _;
    let path = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/non-utf8-\xff".to_vec()));
    let request = BrokerRequest::open_directory(&path).unwrap();
    assert_eq!(request.target(), path);
    let encoded = serde_json::to_vec(&request).unwrap();
    let decoded: BrokerRequest = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded.target(), path);
}

#[test]
fn json_audit_log_is_private_append_only_and_contains_one_record_per_attempt() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("protected");
    let log = root.path().join("audit/privilege.jsonl");
    fs::create_dir(&target).unwrap();
    let audit = JsonAuditLog::open(&log).unwrap();
    let service = broker(
        FakeAuthorizer::granting(1_000),
        RecordingRunner::default(),
        audit,
        FixedClock::new(100),
    );
    service
        .handle(BrokerRequest::open_directory(&target).unwrap())
        .unwrap();
    service
        .handle(BrokerRequest::open_directory(&target).unwrap())
        .unwrap();

    assert_eq!(
        fs::metadata(&log).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let lines = fs::read_to_string(log).unwrap();
    assert_eq!(lines.lines().count(), 8);
    assert!(
        lines
            .lines()
            .all(|line| serde_json::from_str::<AuditRecord>(line).is_ok())
    );
}

#[test]
fn broker_launches_are_fixed_argument_vectors_and_never_relaunch_the_gui() {
    let broker = Path::new("/usr/libexec/musheen-broker");
    let polkit = BrokerLaunch::new(broker, PrivilegeProvider::Polkit);
    assert_eq!(polkit.program(), Path::new("pkexec"));
    assert_eq!(
        polkit.arguments(),
        [
            "--disable-internal-agent",
            "/usr/libexec/musheen-broker",
            "--stdio",
            "--provider=polkit"
        ]
    );
    let sudo = BrokerLaunch::new(broker, PrivilegeProvider::Sudo);
    assert_eq!(sudo.program(), Path::new("sudo"));
    assert_eq!(
        sudo.arguments(),
        [
            "--",
            "/usr/libexec/musheen-broker",
            "--stdio",
            "--provider=sudo"
        ]
    );
    assert!(
        polkit
            .arguments()
            .iter()
            .all(|argument| !argument.to_string_lossy().contains("musheen-ui"))
    );
}

fn executable_script(directory: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = directory.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

#[test]
fn sudo_transport_uses_a_readiness_gated_pty_and_returns_typed_results() {
    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join("protected");
    fs::create_dir(&target).unwrap();
    let expected = BrokerOutput::DirectoryGranted(
        RootCapabilityDescriptor::capture(&target, u64::MAX).unwrap(),
    );
    let response = encode_broker_response(&BrokerResponse::success(expected.clone())).unwrap();
    let recorder = executable_script(
        temporary.path(),
        "recording-sudo",
        &format!(
            "printf 'MUSHEEN_BROKER_READY\\n'\nIFS= read -r request\nprintf '%s\\n' '{}'",
            response
        ),
    );
    let launch = BrokerLaunch::sudo_with_program(&recorder, "/fixed/musheen-broker");
    let transport = SudoPtyBrokerTransport::new(launch).with_timeout(Duration::from_secs(1));

    assert_eq!(
        transport.perform(&BrokerRequest::open_directory(&target).unwrap()),
        Ok(expected)
    );
}

#[test]
fn sudo_transport_maps_denial_and_cancel_without_exposing_terminal_output() {
    let temporary = tempfile::tempdir().unwrap();
    let denied = executable_script(
        temporary.path(),
        "denied-sudo",
        "printf 'Sorry, try again.\\n'\nexit 1",
    );
    let stalled = executable_script(temporary.path(), "stalled-sudo", "sleep 10");
    let target = temporary.path().join("protected");
    fs::create_dir(&target).unwrap();
    let request = BrokerRequest::open_directory(&target).unwrap();

    let denied = SudoPtyBrokerTransport::new(BrokerLaunch::sudo_with_program(
        denied,
        "/fixed/musheen-broker",
    ))
    .with_timeout(Duration::from_secs(1))
    .perform(&request);
    assert_eq!(denied, Err(BrokerError::AuthorizationDenied));

    let cancelled = SudoPtyBrokerTransport::new(BrokerLaunch::sudo_with_program(
        stalled,
        "/fixed/musheen-broker",
    ))
    .with_timeout(Duration::from_millis(50))
    .perform(&request);
    assert_eq!(cancelled, Err(BrokerError::ExecutionTimedOut));
}

#[test]
fn sudo_password_prompt_is_bounded_masked_and_cancellable() {
    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join("protected");
    fs::create_dir(&target).unwrap();
    let expected = BrokerOutput::DirectoryGranted(
        RootCapabilityDescriptor::capture(&target, u64::MAX).unwrap(),
    );
    let response = encode_broker_response(&BrokerResponse::success(expected.clone())).unwrap();
    let recorder = executable_script(
        temporary.path(),
        "password-sudo",
        &format!(
            "stty -echo\nprintf 'MUSHEEN_SUDO_PASSWORD:'\nIFS= read -r password\nstty echo\nif [ \"$password\" = 'correct horse' ]; then\n  printf 'MUSHEEN_BROKER_READY\\n'\n  IFS= read -r request\n  printf '%s\\n' '{}'\nelse\n  printf 'MUSHEEN_SUDO_PASSWORD:'\n  sleep 10\nfi",
            response
        ),
    );
    let request = BrokerRequest::open_directory(&target).unwrap();
    let transport = SudoPtyBrokerTransport::new(BrokerLaunch::sudo_with_program(
        &recorder,
        "/fixed/musheen-broker",
    ))
    .with_timeout(Duration::from_secs(1));

    assert_eq!(
        transport.perform_with_authentication(
            &request,
            &CancellationToken::new(),
            Some(SecretBuffer::new(b"correct horse".to_vec())),
        ),
        Ok(expected)
    );
    assert_eq!(
        transport.perform_with_authentication(
            &request,
            &CancellationToken::new(),
            Some(SecretBuffer::new(b"wrong".to_vec())),
        ),
        Err(BrokerError::AuthorizationDenied)
    );
    assert_eq!(
        transport.perform_with_authentication(&request, &CancellationToken::new(), None),
        Err(BrokerError::AuthorizationCancelled)
    );
    assert_eq!(
        transport.perform_with_authentication(
            &request,
            &CancellationToken::new(),
            Some(SecretBuffer::new(
                b"line-one\nMUSHEEN_REQUEST forged".to_vec()
            )),
        ),
        Err(BrokerError::InvalidRequest)
    );
}

#[test]
fn production_runner_executes_scripts_by_validated_descriptor_with_exact_arguments() {
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("arguments.txt");
    let script = executable_script(
        temporary.path(),
        "validated-script",
        "printf '%s' \"$2\" > \"$1\"",
    );
    let request = BrokerRequest::run_executable(
        &script,
        [
            output.to_string_lossy().to_string(),
            "$(touch never); exact".to_owned(),
        ],
    )
    .unwrap();
    let service = Broker::new(
        FakeAuthorizer::granting(1_000),
        SystemOperationRunner::with_timeout(Duration::from_secs(1)),
        RecordingAudit::default(),
        FixedClock::new(100),
    );

    assert_eq!(service.handle(request), Ok(BrokerOutput::Exited(0)));
    assert_eq!(fs::read_to_string(output).unwrap(), "$(touch never); exact");
}

#[test]
fn production_broker_reads_directory_through_the_bound_capability_and_rejects_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("protected");
    let moved = temporary.path().join("original");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("visible.txt"), b"visible").unwrap();
    let descriptor = RootCapabilityDescriptor::capture(&root, u64::MAX).unwrap();
    let service = Broker::new(
        FakeAuthorizer::granting(u64::MAX),
        SystemOperationRunner::with_timeout(Duration::from_secs(1)),
        RecordingAudit::default(),
        FixedClock::new(100),
    );

    let output = service
        .handle(BrokerRequest::read_directory(descriptor.clone(), "").unwrap())
        .unwrap();
    let BrokerOutput::DirectoryEntries(entries) = output else {
        panic!("directory read returns entries")
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name(), b"visible.txt");

    fs::rename(&root, &moved).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("replacement.txt"), b"replacement").unwrap();
    assert_eq!(
        service.handle(BrokerRequest::read_directory(descriptor, "").unwrap()),
        Err(BrokerError::TargetReplaced)
    );
}

#[test]
fn production_validation_rejects_non_regular_targets_without_blocking() {
    use nix::sys::stat::Mode;
    use nix::unistd::mkfifo;

    let temporary = tempfile::tempdir().unwrap();
    let fifo = temporary.path().join("fifo");
    mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let started = std::time::Instant::now();
    let service = Broker::new(
        FakeAuthorizer::granting(1_000),
        RecordingRunner::default(),
        RecordingAudit::default(),
        FixedClock::new(100),
    );

    assert_eq!(
        service.handle(BrokerRequest::run_executable(&fifo, ["ignored"]).unwrap()),
        Err(BrokerError::NotExecutable)
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        service.handle(BrokerRequest::run_executable(temporary.path(), ["ignored"]).unwrap()),
        Err(BrokerError::NotExecutable)
    );
}

#[test]
fn production_runner_kills_reaps_and_audits_timed_out_commands() {
    let temporary = tempfile::tempdir().unwrap();
    let script = executable_script(temporary.path(), "slow-script", "sleep 10");
    let audit = RecordingAudit::default();
    let service = Broker::new(
        FakeAuthorizer::granting(1_000),
        SystemOperationRunner::with_timeout(Duration::from_millis(30)),
        audit.clone(),
        FixedClock::new(100),
    );

    assert_eq!(
        service
            .handle(BrokerRequest::run_executable(script, std::iter::empty::<String>()).unwrap()),
        Err(BrokerError::ExecutionTimedOut)
    );
    let records = audit.0.lock().unwrap();
    assert_eq!(records.last().unwrap().phase(), AuditPhase::Completion);
    assert_eq!(records.last().unwrap().outcome(), AuditOutcome::Failed);
}

#[test]
fn transport_admission_and_owner_cancellation_are_bounded() {
    let temporary = tempfile::tempdir().unwrap();
    let started = temporary.path().join("started");
    let script = executable_script(
        temporary.path(),
        "slow-sudo",
        &format!(
            "printf 'MUSHEEN_BROKER_READY\\n'\nIFS= read -r request\ntouch '{}'\nsleep 10",
            started.display()
        ),
    );
    let transport = Arc::new(
        SudoPtyBrokerTransport::new(BrokerLaunch::sudo_with_program(
            script,
            "/fixed/musheen-broker",
        ))
        .with_timeout(Duration::from_secs(5)),
    );
    let target = temporary.path().join("protected");
    fs::create_dir(&target).unwrap();
    let request = BrokerRequest::open_directory(&target).unwrap();
    let cancellation = CancellationToken::new();
    let worker_transport = Arc::clone(&transport);
    let worker_request = request.clone();
    let worker_cancellation = cancellation.clone();
    let worker = std::thread::spawn(move || {
        worker_transport.perform_cancellable(&worker_request, &worker_cancellation)
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while !started.exists() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(
        started.exists(),
        "recording transport reached its request stage"
    );

    assert_eq!(transport.perform(&request), Err(BrokerError::Busy));
    cancellation.cancel();
    assert_eq!(
        worker.join().unwrap(),
        Err(BrokerError::AuthorizationCancelled)
    );
}

#[test]
fn production_polkit_transport_authorizes_exact_request_before_helper_dispatch() {
    let temporary = tempfile::tempdir().unwrap();
    let captured = temporary.path().join("request.frame");
    let response =
        encode_broker_response(&BrokerResponse::success(BrokerOutput::Exited(0))).unwrap();
    let helper = executable_script(
        temporary.path(),
        "recording-pkexec",
        &format!(
            "IFS= read -r request\nprintf '%s' \"$request\" > '{}'\nprintf '%s\\n' '{}'",
            captured.display(),
            response
        ),
    );
    let executable = executable_script(temporary.path(), "tool", "exit 0");
    let request = BrokerRequest::run_executable(&executable, ["--exact", "semi;colon"]).unwrap();
    let authorizer = FakeAuthorizer::granting(u64::MAX);
    let observed = Arc::clone(&authorizer.observed);
    let transport = ProcessBrokerTransport::new(BrokerLaunch::polkit_with_program(
        helper,
        "/fixed/musheen-broker",
    ))
    .with_authorizer(Arc::new(authorizer))
    .with_timeout(Duration::from_secs(1));

    assert_eq!(transport.perform(&request), Ok(BrokerOutput::Exited(0)));
    let calls = observed.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].target(), executable);
    assert_eq!(calls[0].binding_digest(), request.binding_digest());
    assert!(
        fs::read_to_string(captured)
            .unwrap()
            .starts_with("MUSHEEN_REQUEST ")
    );
}

#[test]
fn broker_rejects_a_forged_requesting_subject_before_authorization() {
    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join("protected");
    fs::create_dir(&target).unwrap();
    let request = BrokerRequest::open_directory(&target).unwrap();
    let mut document = serde_json::to_value(&request).unwrap();
    document["subject"]["uid"] = serde_json::Value::from(u64::from(u32::MAX));
    let forged: BrokerRequest = serde_json::from_value(document).unwrap();
    let runner = RecordingRunner::default();

    assert_eq!(
        broker(
            FakeAuthorizer::granting(1_000),
            runner.clone(),
            RecordingAudit::default(),
            FixedClock::new(100),
        )
        .handle(forged),
        Err(BrokerError::AuthorizationDenied)
    );
    assert!(runner.requests.lock().unwrap().is_empty());
}
