use musheen_desktop::Clock as _;
use musheen_desktop::privilege::{
    AuthorizationError, AuthorizationGrant, AuthorizationRequest, Authorizer,
    BROKER_PROTOCOL_ARGUMENT, BROKER_PROTOCOL_VERSION, Broker, BrokerOperation, BrokerOutput,
    BrokerRequest, BrokerResponse, ELEVATED_SESSION_IDLE, JsonAuditLog, PrivilegeProvider,
    RequestLines, SUDO_BROKER_READY, SystemClock, SystemOperationRunner, boot_clock,
    decode_broker_request, encode_ownership_progress, prepare_sudo_terminal, serve_session,
    write_protocol_version, write_response,
};
use std::io::Write as _;
use std::path::PathBuf;
const AUDIT_PATH: &str = "/var/log/musheen/privilege.jsonl";

enum ElevatedBrokerAuthorizer {
    Pkexec,
    Sudo,
}

impl Authorizer for ElevatedBrokerAuthorizer {
    fn authorize(
        &self,
        request: &AuthorizationRequest,
    ) -> Result<AuthorizationGrant, AuthorizationError> {
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(AuthorizationError::Denied);
        }
        let variable = match self {
            Self::Pkexec => "PKEXEC_UID",
            Self::Sudo => "SUDO_UID",
        };
        let invoking_uid = std::env::var(variable)
            .ok()
            .and_then(|uid| uid.parse::<u32>().ok())
            .ok_or(AuthorizationError::Denied)?;
        if invoking_uid != request.subject().uid() {
            return Err(AuthorizationError::Denied);
        }
        Ok(AuthorizationGrant::new(
            format!("uid:{invoking_uid}"),
            SystemClock.now_unix_millis().saturating_add(60_000),
        ))
    }
}

fn main() {
    // Musheen runs the installed broker without privileges to read its
    // protocol version before it asks for authorization (SYS-034).
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() == Some(std::ffi::OsStr::new(BROKER_PROTOCOL_ARGUMENT))
        && arguments.next().is_none()
    {
        let _ = write_protocol_version(&mut std::io::stdout().lock(), BROKER_PROTOCOL_VERSION);
        return;
    }
    let invocation = match parse_invocation() {
        Some(invocation) => invocation,
        None => return fail("invalid broker invocation", 2),
    };
    let provider = invocation.provider;
    if provider == PrivilegeProvider::Sudo {
        let mut stdout = std::io::stdout().lock();
        if prepare_sudo_terminal().is_err()
            || writeln!(stdout, "{SUDO_BROKER_READY}")
                .and_then(|()| stdout.flush())
                .is_err()
        {
            return fail("broker transport unavailable", 3);
        }
    }
    // The broker names its protocol version before it reads anything, so
    // Musheen refuses a broker that replaced the one it checked before it
    // sends a request (SYS-034).
    if write_protocol_version(&mut std::io::stdout().lock(), BROKER_PROTOCOL_VERSION).is_err() {
        return fail("broker transport unavailable", 3);
    }
    let Ok(requests) = RequestLines::spawn(std::io::stdin()) else {
        return fail("broker transport unavailable", 3);
    };
    let mut request = match requests
        .first()
        .and_then(Result::ok)
        .and_then(|frame| decode_broker_request(frame.trim()).ok())
    {
        Some(request) => request,
        None => return fail("invalid broker request", 2),
    };
    // A folder listing runs only inside the session of the Open as
    // Administrator request that authorized its root (SYS-034).
    if !invocation.matches(&request)
        || matches!(request.operation(), BrokerOperation::ReadDirectory { .. })
    {
        return fail("request binding mismatch", 2);
    }
    let environment: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let parent_pid =
        match rustix::process::getppid().and_then(|pid| u32::try_from(pid.as_raw_pid()).ok()) {
            Some(pid) => pid,
            None => return fail("caller identity unavailable", 3),
        };
    let bind =
        |request: &mut BrokerRequest| request.bind_to_invoker(provider, &environment, parent_pid);
    if bind(&mut request).is_err() {
        return fail("caller identity unavailable", 3);
    }
    if rustix::process::geteuid().as_raw() != 0 {
        return fail("broker must run elevated", 3);
    }
    let audit = match JsonAuditLog::open(AUDIT_PATH) {
        Ok(audit) => audit,
        Err(_) => return fail("broker audit unavailable", 3),
    };
    let authorizer = match provider {
        PrivilegeProvider::Polkit => ElevatedBrokerAuthorizer::Pkexec,
        PrivilegeProvider::Sudo => ElevatedBrokerAuthorizer::Sudo,
    };
    // An ownership change reports its progress on the output, and stops
    // at once when Musheen closes the input, as Musheen cannot signal a
    // broker running as root (SYS-037).
    let runner = if matches!(request.operation(), BrokerOperation::ChangeOwnership { .. }) {
        requests.end_with_input();
        SystemOperationRunner::default().with_progress(std::sync::Arc::new(|changed, path| {
            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(stdout, "{}", encode_ownership_progress(changed, path));
            let _ = stdout.flush();
        }))
    } else {
        SystemOperationRunner::default()
    };
    let broker = Broker::new(authorizer, runner, audit, SystemClock).with_provider(provider);
    let mut stdout = std::io::stdout();
    match broker.handle(request) {
        Ok(BrokerOutput::RootReferenced(root)) => {
            let answered = write_response(
                &mut stdout,
                &BrokerResponse::success(BrokerOutput::RootReferenced(root.clone())),
            );
            if answered.is_ok() {
                serve_session(
                    &broker,
                    &root,
                    &requests,
                    &mut stdout,
                    &bind,
                    ELEVATED_SESSION_IDLE,
                    &boot_clock,
                );
            }
            // The input thread may still wait on stdin.
            std::process::exit(0);
        }
        Ok(output) => {
            let _ = write_response(&mut stdout, &BrokerResponse::success(output));
            std::process::exit(0);
        }
        Err(error) => {
            let _ = write_response(&mut stdout, &BrokerResponse::failure(&error));
            std::process::exit(1);
        }
    }
}

struct InvocationBinding {
    provider: PrivilegeProvider,
    action_id: String,
    request_digest: String,
    target: PathBuf,
}

impl InvocationBinding {
    fn matches(&self, request: &musheen_desktop::privilege::BrokerRequest) -> bool {
        self.action_id == request.operation().action_id()
            && self.request_digest == request.operation_digest().to_hex().as_str()
            && self.target == request.target()
    }
}

fn parse_invocation() -> Option<InvocationBinding> {
    let mut arguments = std::env::args_os().skip(1);
    let action_id = arguments
        .next()?
        .to_str()?
        .strip_prefix("--action-id=")?
        .to_owned();
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--stdio")) {
        return None;
    }
    let provider = match arguments.next()?.to_str()? {
        "--provider=polkit" => PrivilegeProvider::Polkit,
        "--provider=sudo" => PrivilegeProvider::Sudo,
        _ => return None,
    };
    let request_digest = arguments
        .next()?
        .to_str()?
        .strip_prefix("--request-digest=")?
        .to_owned();
    if request_digest.len() != 64
        || !request_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--target")) {
        return None;
    }
    let target = PathBuf::from(arguments.next()?);
    if arguments.next().is_some() {
        return None;
    }
    Some(InvocationBinding {
        provider,
        action_id,
        request_digest,
        target,
    })
}

fn fail(message: &str, code: i32) {
    eprintln!("{message}");
    std::process::exit(code);
}
