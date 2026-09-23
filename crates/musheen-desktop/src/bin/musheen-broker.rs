use musheen_desktop::Clock as _;
use musheen_desktop::privilege::{
    AuthorizationError, AuthorizationGrant, AuthorizationRequest, Authorizer, Broker,
    BrokerResponse, JsonAuditLog, PolkitAuthorizer, PrivilegeProvider, SUDO_BROKER_READY,
    SystemClock, SystemOperationRunner, decode_broker_request, encode_broker_response,
};
use std::io::{BufRead as _, Read as _, Write as _};
use std::path::PathBuf;

const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const AUDIT_PATH: &str = "/var/log/musheen/privilege.jsonl";

enum ElevatedBrokerAuthorizer {
    Polkit(PolkitAuthorizer),
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
        match self {
            Self::Polkit(authorizer) => authorizer.authorize(request),
            Self::Sudo => {
                let invoking_uid = std::env::var("SUDO_UID")
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
    }
}

fn main() {
    let invocation = match parse_invocation() {
        Some(invocation) => invocation,
        None => return fail("invalid broker invocation", 2),
    };
    let provider = invocation.provider;
    if provider == PrivilegeProvider::Sudo {
        let mut stdout = std::io::stdout().lock();
        if writeln!(stdout, "{SUDO_BROKER_READY}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            return fail("broker transport unavailable", 3);
        }
    }
    let stdin = std::io::stdin();
    let mut request = Vec::new();
    if stdin
        .lock()
        .take(MAX_REQUEST_BYTES + 1)
        .read_until(b'\n', &mut request)
        .is_err()
        || request.len() as u64 > MAX_REQUEST_BYTES
    {
        return fail("invalid broker request", 2);
    }
    let mut request = match std::str::from_utf8(&request)
        .map_err(|_| ())
        .and_then(|frame| decode_broker_request(frame.trim()).map_err(|_| ()))
    {
        Ok(request) => request,
        Err(_) => return fail("invalid broker request", 2),
    };
    if !invocation.matches(&request) {
        return fail("request binding mismatch", 2);
    }
    let environment = std::env::vars().collect();
    let parent_pid =
        match rustix::process::getppid().and_then(|pid| u32::try_from(pid.as_raw_pid()).ok()) {
            Some(pid) => pid,
            None => return fail("caller identity unavailable", 3),
        };
    if request
        .bind_to_invoker(provider, &environment, parent_pid)
        .is_err()
    {
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
        PrivilegeProvider::Polkit => ElevatedBrokerAuthorizer::Polkit(
            PolkitAuthorizer::default().with_user_interaction(false),
        ),
        PrivilegeProvider::Sudo => ElevatedBrokerAuthorizer::Sudo,
    };
    let broker = Broker::new(
        authorizer,
        SystemOperationRunner::default(),
        audit,
        SystemClock,
    )
    .with_provider(provider);
    match broker.handle(request) {
        Ok(output) => write_response(BrokerResponse::success(output)),
        Err(error) => {
            write_response(BrokerResponse::failure(&error));
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
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--stdio")) {
        return None;
    }
    let provider = match arguments.next()?.to_str()? {
        "--provider=polkit" => PrivilegeProvider::Polkit,
        "--provider=sudo" => PrivilegeProvider::Sudo,
        _ => return None,
    };
    let action_id = arguments
        .next()?
        .to_str()?
        .strip_prefix("--action-id=")?
        .to_owned();
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

fn write_response(response: BrokerResponse) {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    if let Ok(frame) = encode_broker_response(&response) {
        let _ = writeln!(stdout, "{frame}");
    }
}

fn fail(message: &str, code: i32) {
    eprintln!("{message}");
    std::process::exit(code);
}
