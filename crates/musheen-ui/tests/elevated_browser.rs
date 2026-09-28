use musheen_core::{CancellationToken, PageRequest, ResourceLimits, Store, StorePath};
use musheen_desktop::SecretBuffer;
use musheen_desktop::privilege::{
    AuthorizationError, AuthorizationGrant, AuthorizationRequest, Authorizer,
    BROKER_PROTOCOL_VERSION, Broker, BrokerError, BrokerLaunch, BrokerOutput, BrokerRequest,
    BrokerResponse, BrokerTransport, ELEVATED_SESSION_IDLE, ElevatedRootReference, NoopAudit,
    OwnershipContents, OwnershipItem, OwnershipReport, ProcessBrokerTransport, RequestLines,
    SUDO_BROKER_READY, SudoPtyBrokerTransport, SystemClock, SystemOperationRunner, boot_clock,
    decode_broker_request, prepare_sudo_terminal, serve_session, write_protocol_version,
    write_response,
};
use musheen_desktop::{Clock, PrivilegeProvider, RootGrant, RootedStore};
use musheen_ui::{
    AppearanceMode, Catalog, ElevatedBrowser, ElevatedSession, Locale, PrivilegeBackend,
    RootedFilesystemStore, SystemPrivilegeBackend,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

#[derive(Clone)]
struct ClockAt(u64);

impl Clock for ClockAt {
    fn now_unix_millis(&self) -> u64 {
        self.0
    }
}

fn browser(root: &Path, locale: Locale) -> ElevatedBrowser<ClockAt> {
    let grant = RootGrant::open(root, "grant", 1_000, PrivilegeProvider::Polkit).unwrap();
    ElevatedBrowser::new(
        RootedStore::new(grant, ClockAt(100)),
        Catalog::load(locale).unwrap(),
    )
}

#[test]
fn warning_and_privilege_icon_are_permanent_localized_and_theme_independent() {
    let root = tempfile::tempdir().unwrap();
    for locale in [Locale::EnUs, Locale::EnXa, Locale::Ar] {
        let browser = browser(root.path(), locale);
        let expected = Catalog::load(locale)
            .unwrap()
            .message("elevated-browser-warning")
            .unwrap()
            .to_owned();
        for mode in [
            AppearanceMode::Light,
            AppearanceMode::Dark,
            AppearanceMode::HighContrast,
        ] {
            let chrome = browser.chrome(mode);
            assert_eq!(chrome.warning(), expected);
            assert_eq!(chrome.icon(), "shield");
            assert!(chrome.always_visible());
            assert!(chrome.distinct_from_ordinary_chrome());
            assert!(chrome.accessible_name().contains(&expected));
        }
    }
}

#[test]
fn navigation_and_breadcrumbs_remain_inside_the_granted_root() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("one/two")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("one/escape")).unwrap();
    let mut browser = browser(root.path(), Locale::EnUs);

    browser.navigate(Path::new("one/two")).unwrap();
    assert_eq!(browser.relative_location(), Path::new("one/two"));
    browser.navigate_breadcrumb(0).unwrap();
    assert_eq!(browser.relative_location(), Path::new("one"));
    assert!(browser.navigate(Path::new("../outside")).is_err());
    assert!(browser.navigate(Path::new("one/escape")).is_err());
    assert_eq!(browser.relative_location(), Path::new("one"));
}

#[test]
fn elevated_store_enumerates_through_the_granted_descriptor_after_root_path_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("protected");
    let moved = temporary.path().join("original");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("original.txt"), b"original").unwrap();
    let grant = RootGrant::open(&root, "grant", 1_000, PrivilegeProvider::Polkit).unwrap();
    let store = RootedFilesystemStore::new(RootedStore::new(grant, ClockAt(100)));

    fs::rename(&root, &moved).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("replacement.txt"), b"replacement").unwrap();
    let page = futures_lite::future::block_on(store.read_directory(
        &StorePath::from_unix_path(root.as_os_str()),
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ))
    .unwrap();

    assert_eq!(page.items().len(), 1);
    assert_eq!(page.items()[0].display_name().as_str(), "original.txt");
    assert!(
        page.items()
            .iter()
            .all(|item| item.display_name().as_str() != "replacement.txt")
    );
}

// SYS-034: one broker session for each elevated window. The fake pkexec and
// sudo below stand in for authorization: each run is one authorization. They
// start this test binary as the broker (`elevated_session_broker_child`),
// which runs unprivileged with an authorizer that allows the invoking user.

const BROKER_ARGUMENTS: &str = "MUSHEEN_SESSION_BROKER_ARGUMENTS";
const BROKER_PIDS: &str = "MUSHEEN_SESSION_BROKER_PIDS";
const BROKER_IDLE_MILLIS: &str = "MUSHEEN_SESSION_BROKER_IDLE_MILLIS";
const BROKER_PROBES: &str = "MUSHEEN_SESSION_BROKER_PROBES";
const BROKER_LISTINGS: &str = "MUSHEEN_SESSION_BROKER_LISTINGS";
const BROKER_SUSPEND: &str = "MUSHEEN_SESSION_BROKER_SUSPEND";
/// The protocol version the installed broker reports before authorization.
const BROKER_INSTALLED_PROTOCOL: &str = "MUSHEEN_SESSION_BROKER_INSTALLED_PROTOCOL";
/// The protocol version the elevated broker names in its first answer.
const BROKER_ANSWER_PROTOCOL: &str = "MUSHEEN_SESSION_BROKER_ANSWER_PROTOCOL";
/// With "1", the broker's first listing answer cannot be decoded.
const BROKER_GARBLE: &str = "MUSHEEN_SESSION_BROKER_GARBLE";
const PASSWORD: &[u8] = b"correct horse";

/// Allows the invoking user, as pkexec or sudo does once it authenticated.
struct AllowInvoker;

impl Authorizer for AllowInvoker {
    fn authorize(
        &self,
        _request: &AuthorizationRequest,
    ) -> Result<AuthorizationGrant, AuthorizationError> {
        Ok(AuthorizationGrant::new(
            format!("uid:{}", rustix::process::getuid().as_raw()),
            SystemClock.now_unix_millis() + 60_000,
        ))
    }
}

/// Tries, from inside the broker, what another process of the user would try
/// to reach the session, and returns one `probe=opened|refused` line each.
/// Polkit: reopening the broker's input through /proc, after reopening an
/// ordinary pipe the same way to show the probe works. Sudo: opening the
/// broker's terminal by name as Musheen left it, again with its lock cleared
/// to show the probe works, and after the broker prepared it.
fn probe_channel(provider: PrivilegeProvider) -> Vec<String> {
    use rustix::fs::{Mode, OFlags};
    use std::os::fd::AsRawFd as _;

    let outcome = |opened: bool| if opened { "opened" } else { "refused" };
    let reopen = |fd: i32| {
        fs::OpenOptions::new()
            .write(true)
            .open(format!("/proc/self/fd/{fd}"))
            .is_ok()
    };
    match provider {
        PrivilegeProvider::Polkit => {
            let (pipe, _writer) = std::io::pipe().unwrap();
            vec![
                format!("pipe={}", outcome(reopen(pipe.as_raw_fd()))),
                format!("input={}", outcome(reopen(0))),
            ]
        }
        PrivilegeProvider::Sudo => {
            let terminal = fs::read_link("/proc/self/fd/0").unwrap();
            let open_terminal = || {
                rustix::fs::open(
                    terminal.as_path(),
                    OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .is_ok()
            };
            let musheen = outcome(open_terminal());
            rustix::termios::ioctl_tiocnxcl(std::io::stdin()).unwrap();
            let unlocked = outcome(open_terminal());
            prepare_sudo_terminal().unwrap();
            let broker = outcome(open_terminal());
            vec![
                format!("musheen={musheen}"),
                format!("unlocked={unlocked}"),
                format!("broker={broker}"),
            ]
        }
    }
}

/// Plays musheen-broker when a fake pkexec or sudo starts this test binary,
/// and does nothing otherwise. It runs the broker's own session code, with an
/// authorizer that allows the invoking user.
#[test]
fn elevated_session_broker_child() {
    use std::io::Write as _;

    let Ok(arguments) = std::env::var(BROKER_ARGUMENTS) else {
        return;
    };
    // Musheen reads the installed broker's version before it asks for
    // authorization, by running it without privileges.
    let version = |variable: &str| {
        std::env::var(variable)
            .ok()
            .and_then(|version| version.parse().ok())
            .unwrap_or(BROKER_PROTOCOL_VERSION)
    };
    if arguments.contains("--protocol-version") {
        write_protocol_version(&mut std::io::stdout(), version(BROKER_INSTALLED_PROTOCOL)).unwrap();
        return;
    }
    let provider = if arguments.contains("--provider=sudo") {
        PrivilegeProvider::Sudo
    } else {
        PrivilegeProvider::Polkit
    };
    if let Ok(pids) = std::env::var(BROKER_PIDS) {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(pids)
            .unwrap();
        writeln!(file, "{}", std::process::id()).unwrap();
    }
    let idle = std::env::var(BROKER_IDLE_MILLIS)
        .ok()
        .and_then(|millis| millis.parse().ok())
        .map_or(ELEVATED_SESSION_IDLE, Duration::from_millis);
    if let Ok(probes) = std::env::var(BROKER_PROBES) {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(probes)
            .unwrap();
        for line in probe_channel(provider) {
            writeln!(file, "{line}").unwrap();
        }
    }
    if provider == PrivilegeProvider::Sudo {
        prepare_sudo_terminal().unwrap();
        println!("{SUDO_BROKER_READY}");
    }
    write_protocol_version(&mut std::io::stdout(), version(BROKER_ANSWER_PROTOCOL)).unwrap();
    let requests = RequestLines::spawn(std::io::stdin()).unwrap();
    let first = requests.first().unwrap().unwrap();
    let mut request = decode_broker_request(first.trim()).unwrap();
    let environment = std::env::vars().collect();
    let parent = std::os::unix::process::parent_id();
    let bind =
        |request: &mut BrokerRequest| request.bind_to_invoker(provider, &environment, parent);
    bind(&mut request).unwrap();
    let broker = Broker::new(
        AllowInvoker,
        SystemOperationRunner::default(),
        NoopAudit,
        SystemClock,
    )
    .with_provider(provider);
    let mut output = std::io::stdout();
    let opened = broker.handle(request);
    write_response(
        &mut output,
        &opened.clone().map_or_else(
            |error| BrokerResponse::failure(&error),
            BrokerResponse::success,
        ),
    )
    .unwrap();
    // Each listing is counted. With BROKER_SUSPEND, the machine is suspended
    // as the first listing arrives: the boot clock jumps past the idle limit
    // while the monotonic clock does not move.
    let listings = std::env::var(BROKER_LISTINGS).ok();
    let suspend = std::env::var(BROKER_SUSPEND).is_ok_and(|value| value == "1");
    let suspended = std::sync::atomic::AtomicBool::new(false);
    let session_bind = |request: &mut BrokerRequest| {
        if let Some(listings) = &listings {
            let mut file = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(listings)
                .unwrap();
            writeln!(file, "listing").unwrap();
        }
        if suspend {
            suspended.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        bind(request)
    };
    let clock = || {
        if suspended.load(std::sync::atomic::Ordering::SeqCst) {
            boot_clock() + ELEVATED_SESSION_IDLE + Duration::from_secs(60)
        } else {
            boot_clock()
        }
    };
    let mut output = GarbleFirstLine {
        inner: output,
        line: Vec::new(),
        done: std::env::var(BROKER_GARBLE).map_or(true, |garble| garble != "1"),
    };
    if let Ok(BrokerOutput::RootReferenced(root)) = opened {
        serve_session(
            &broker,
            &root,
            &requests,
            &mut output,
            &session_bind,
            idle,
            &clock,
        );
    }
}

/// Passes the broker's output on, except that its first line becomes an
/// answer that cannot be decoded, unless `done` is set.
struct GarbleFirstLine<W: std::io::Write> {
    inner: W,
    line: Vec<u8>,
    done: bool,
}

impl<W: std::io::Write> std::io::Write for GarbleFirstLine<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.done {
            return self.inner.write(bytes);
        }
        self.line.extend_from_slice(bytes);
        if let Some(newline) = self.line.iter().position(|byte| *byte == b'\n') {
            self.done = true;
            self.inner.write_all(b"MUSHEEN_RESPONSE {not an answer\n")?;
            let rest = self.line.split_off(newline + 1);
            self.inner.write_all(&rest)?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn executable_script(directory: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = directory.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// A fake pkexec or sudo that counts its runs and starts the child broker.
struct FakeElevation {
    provider: PrivilegeProvider,
    directory: tempfile::TempDir,
    launch: BrokerLaunch,
}

impl FakeElevation {
    fn new(provider: PrivilegeProvider, idle: Duration) -> Self {
        Self::with_suspend(provider, idle, false)
    }

    /// A fake whose broker sees the machine suspended past its idle limit
    /// right after its first listing.
    fn suspending(provider: PrivilegeProvider) -> Self {
        Self::with_suspend(provider, IDLE, true)
    }

    fn with_suspend(provider: PrivilegeProvider, idle: Duration, suspend: bool) -> Self {
        Self::build(provider, idle, suspend, "")
    }

    /// A fake whose broker also gets `environment`, as `NAME='value'`
    /// assignments.
    fn with_broker_environment(provider: PrivilegeProvider, environment: &str) -> Self {
        Self::build(provider, IDLE, false, environment)
    }

    fn build(
        provider: PrivilegeProvider,
        idle: Duration,
        suspend: bool,
        environment: &str,
    ) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let runs = directory.path().join("runs");
        let pids = directory.path().join("pids");
        let probes = directory.path().join("probes");
        let listings = directory.path().join("listings");
        let broker = executable_script(
            directory.path(),
            "broker",
            &format!(
                "{environment} {BROKER_ARGUMENTS}=\"$*\" {BROKER_PIDS}='{}' {BROKER_PROBES}='{}' \
                 {BROKER_LISTINGS}='{}' {BROKER_SUSPEND}='{}' {BROKER_IDLE_MILLIS}='{}' '{}' \
                 elevated_session_broker_child --exact --nocapture --test-threads=1 \
                 | /usr/bin/grep --line-buffered -o 'MUSHEEN_.*'",
                pids.display(),
                probes.display(),
                listings.display(),
                if suspend { "1" } else { "0" },
                idle.as_millis(),
                std::env::current_exe().unwrap().display(),
            ),
        );
        let launch = match provider {
            PrivilegeProvider::Polkit => {
                let pkexec = executable_script(
                    directory.path(),
                    "pkexec",
                    &format!(
                        "echo run >> '{}'\nshift\nPKEXEC_UID=$(/usr/bin/id -u) exec \"$@\"",
                        runs.display()
                    ),
                );
                BrokerLaunch::polkit_with_program(pkexec, broker)
            }
            PrivilegeProvider::Sudo => {
                let sudo = executable_script(
                    directory.path(),
                    "sudo",
                    &format!(
                        "/usr/bin/stty -echo\nprintf 'MUSHEEN_SUDO_PASSWORD:'\n\
                         IFS= read -r password\n/usr/bin/stty echo\n\
                         echo run >> '{}'\n\
                         [ \"$password\" = 'correct horse' ] || exit 1\n\
                         shift\nSUDO_UID=$(/usr/bin/id -u) exec \"$@\"",
                        runs.display()
                    ),
                );
                BrokerLaunch::sudo_with_program(sudo, broker)
            }
        };
        Self {
            provider,
            directory,
            launch,
        }
    }

    /// How many times authorization ran.
    fn runs(&self) -> usize {
        fs::read_to_string(self.directory.path().join("runs"))
            .map(|runs| runs.lines().count())
            .unwrap_or(0)
    }

    /// The process IDs of every broker started.
    fn broker_pids(&self) -> Vec<u32> {
        fs::read_to_string(self.directory.path().join("pids"))
            .map(|pids| pids.lines().map(|pid| pid.parse().unwrap()).collect())
            .unwrap_or_default()
    }

    /// How many folder listings the brokers served.
    fn listings(&self) -> usize {
        fs::read_to_string(self.directory.path().join("listings"))
            .map(|listings| listings.lines().count())
            .unwrap_or(0)
    }

    /// What the brokers' probes found, one `probe=result` line each.
    fn probes(&self) -> Vec<String> {
        fs::read_to_string(self.directory.path().join("probes"))
            .map(|probes| probes.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    fn backend(&self) -> Arc<SystemPrivilegeBackend> {
        let transport: Arc<dyn BrokerTransport> = match self.provider {
            PrivilegeProvider::Polkit => Arc::new(
                ProcessBrokerTransport::new(self.launch.clone())
                    .with_timeout(Duration::from_secs(10)),
            ),
            PrivilegeProvider::Sudo => Arc::new(
                SudoPtyBrokerTransport::new(self.launch.clone())
                    .with_timeout(Duration::from_secs(10)),
            ),
        };
        Arc::new(SystemPrivilegeBackend::with_transport(
            self.provider,
            transport,
        ))
    }

    fn password(&self) -> Option<SecretBuffer> {
        (self.provider == PrivilegeProvider::Sudo).then(|| SecretBuffer::new(PASSWORD.to_vec()))
    }
}

/// Opens `root` as administrator, as Open as Administrator does, and returns
/// the elevated window's store and session.
fn open_window(
    fake: &FakeElevation,
    backend: &Arc<SystemPrivilegeBackend>,
    root: &Path,
) -> (RootedFilesystemStore<SystemClock>, Arc<dyn ElevatedSession>) {
    let request = BrokerRequest::open_directory(root).unwrap();
    let session = futures_lite::future::block_on(backend.open_window(
        &request,
        CancellationToken::new(),
        fake.password(),
    ))
    .expect("Open as Administrator authorizes");
    (RootedFilesystemStore::remote(Arc::clone(&session)), session)
}

/// Lists every page of `folder` through the window's store.
fn list_all(
    store: &RootedFilesystemStore<SystemClock>,
    folder: &Path,
    page_size: usize,
) -> Result<Vec<String>, musheen_core::StoreError> {
    let location = StorePath::from_unix_path(folder.as_os_str());
    let mut names = Vec::new();
    let mut request = PageRequest::new(page_size, None).unwrap();
    loop {
        let page = futures_lite::future::block_on(store.read_directory(
            &location,
            request,
            CancellationToken::new(),
        ))?;
        names.extend(
            page.items()
                .iter()
                .map(|item| item.display_name().as_str().to_owned()),
        );
        match page.next_request() {
            Some(next) => request = next,
            None => return Ok(names),
        }
    }
}

const PROVIDERS: [PrivilegeProvider; 2] = [PrivilegeProvider::Polkit, PrivilegeProvider::Sudo];

/// Held by each test that lists tens of megabytes, so those listings do not
/// run at the same time and slow each other past SYS-034's 10-second bound.
static LARGE_LISTINGS: Mutex<()> = Mutex::new(());

const IDLE: Duration = Duration::from_secs(60);

#[test]
fn elevated_session_authorizes_once_for_a_window_of_listings() {
    for provider in PROVIDERS {
        let fake = FakeElevation::new(provider, IDLE);
        let backend = fake.backend();
        let root = tempfile::tempdir().unwrap();
        for folder in ["a", "b", "c", "many"] {
            fs::create_dir(root.path().join(folder)).unwrap();
        }
        for index in 0..5 {
            fs::write(root.path().join("many").join(format!("{index}.txt")), b"x").unwrap();
        }
        let (store, _session) = open_window(&fake, &backend, root.path());

        for folder in ["", "a", "b", "c"] {
            list_all(&store, &root.path().join(folder), 100)
                .unwrap_or_else(|error| panic!("{provider:?} lists {folder:?}: {error}"));
        }
        let many = list_all(&store, &root.path().join("many"), 2)
            .unwrap_or_else(|error| panic!("{provider:?} lists every page: {error}"));
        assert_eq!(many.len(), 5, "{provider:?}");
        assert_eq!(
            fake.runs(),
            1,
            "{provider:?}: one authorization serves the window's listings and pages"
        );
    }
}

#[test]
fn elevated_session_lists_more_than_the_pipe_holds() {
    for provider in PROVIDERS {
        let fake = FakeElevation::new(provider, IDLE);
        let backend = fake.backend();
        let root = tempfile::tempdir().unwrap();
        for index in 0..2_000 {
            fs::write(
                root.path().join(format!(
                    "a-file-with-a-long-name-for-the-listing-{index:05}"
                )),
                b"",
            )
            .unwrap();
        }
        let (store, _session) = open_window(&fake, &backend, root.path());

        let started = std::time::Instant::now();
        let names = list_all(&store, root.path(), 1_000).unwrap_or_else(|error| {
            panic!(
                "{provider:?} lists 2,000 entries: {error} after {:?}",
                started.elapsed()
            )
        });
        assert_eq!(names.len(), 2_000, "{provider:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{provider:?} took {:?}",
            started.elapsed()
        );
    }
}

#[test]
fn elevated_session_lists_long_names_up_to_the_limit() {
    let _one_at_a_time = LARGE_LISTINGS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    // 64,000 names of 255 bytes, about 16 MiB of names: a listing that
    // sends each byte of a name as a JSON number passes 64 MiB.
    let root = tempfile::tempdir().unwrap();
    for index in 0..64_000 {
        fs::File::create(root.path().join(format!("{index:05}{}", "x".repeat(250)))).unwrap();
    }
    for provider in PROVIDERS {
        let fake = FakeElevation::new(provider, IDLE);
        let backend = fake.backend();
        let (store, _session) = open_window(&fake, &backend, root.path());

        let started = std::time::Instant::now();
        let names = list_all(&store, root.path(), 1_000).unwrap_or_else(|error| {
            panic!(
                "{provider:?} lists 64,000 long names: {error} after {:?}",
                started.elapsed()
            )
        });
        assert_eq!(names.len(), 64_000, "{provider:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{provider:?} took {:?}",
            started.elapsed()
        );
    }
}

#[test]
fn elevated_session_lists_to_the_limit_and_names_it_beyond() {
    let _one_at_a_time = LARGE_LISTINGS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    // Each name of 255 bytes adds 385 bytes to a listing: 170,000 of them
    // make about 65.5 MB, just under 64 MiB, and 180,000 about 69.3 MB.
    let root = tempfile::tempdir().unwrap();
    let create = |names: std::ops::Range<u32>| {
        for index in names {
            fs::File::create(root.path().join(format!("{index:06}{}", "x".repeat(249)))).unwrap();
        }
    };
    let small = root.path().join("small");
    fs::create_dir(&small).unwrap();
    fs::write(small.join("leaf.txt"), b"leaf").unwrap();
    create(0..170_000);
    for provider in PROVIDERS {
        let fake = FakeElevation::new(provider, IDLE);
        let backend = fake.backend();
        let (store, _session) = open_window(&fake, &backend, root.path());

        let started = std::time::Instant::now();
        let names = list_all(&store, root.path(), 1_000).unwrap_or_else(|error| {
            panic!(
                "{provider:?} lists up to the limit: {error} after {:?}",
                started.elapsed()
            )
        });
        assert_eq!(names.len(), 170_001, "{provider:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{provider:?} took {:?}",
            started.elapsed()
        );
    }

    create(170_000..180_000);
    let english = Catalog::load(Locale::EnUs).unwrap();
    let arabic = Catalog::load(Locale::Ar).unwrap();
    for provider in PROVIDERS {
        let fake = FakeElevation::new(provider, IDLE);
        let backend = fake.backend();
        let (store, _session) = open_window(&fake, &backend, root.path());

        let error = list_all(&store, root.path(), 1_000)
            .expect_err("a listing over 64 MiB is refused")
            .to_string();
        assert_eq!(
            error,
            english
                .message("privilege-error-listing-too-large")
                .unwrap(),
            "{provider:?}: the refusal names the limit"
        );
        assert_eq!(
            arabic.localize_reason(&error),
            arabic.message("privilege-error-listing-too-large").unwrap(),
            "{provider:?}: the folder's error view shows it in the user's language"
        );
        let names = list_all(&store, &small, 100)
            .unwrap_or_else(|error| panic!("{provider:?} lists after a refusal: {error}"));
        assert_eq!(names, ["leaf.txt"], "{provider:?}");
        assert_eq!(
            fake.runs(),
            1,
            "{provider:?}: the window keeps its authorization"
        );
    }
}

#[test]
fn elevated_session_lists_a_folder_whose_path_fits_the_limit() {
    for provider in PROVIDERS {
        let fake = FakeElevation::new(provider, IDLE);
        let backend = fake.backend();
        let root = tempfile::tempdir().unwrap();
        let mut deep = root.path().to_path_buf();
        while deep.as_os_str().len() < 3_500 {
            deep.push("d".repeat(200));
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("leaf.txt"), b"leaf").unwrap();
        let (store, _session) = open_window(&fake, &backend, root.path());

        let names = list_all(&store, &deep, 100)
            .unwrap_or_else(|error| panic!("{provider:?} lists a deep folder: {error}"));
        assert_eq!(names, ["leaf.txt"], "{provider:?}");
    }
}

#[test]
fn elevated_session_refuses_what_it_was_not_granted() {
    for provider in PROVIDERS {
        let fake = FakeElevation::new(provider, IDLE);
        let backend = fake.backend();
        let parent = tempfile::tempdir().unwrap();
        let granted = parent.path().join("granted");
        let other = parent.path().join("other");
        fs::create_dir(&granted).unwrap();
        fs::create_dir(&other).unwrap();
        let (store, session) = open_window(&fake, &backend, &granted);
        list_all(&store, &granted, 100).unwrap();

        let outside = futures_lite::future::block_on(session.read_directory(
            ElevatedRootReference::capture(&other).unwrap(),
            PathBuf::new(),
            CancellationToken::new(),
        ));
        assert!(
            outside.is_err(),
            "{provider:?}: a folder outside the grant is refused"
        );

        fs::rename(&granted, parent.path().join("moved")).unwrap();
        fs::create_dir(&granted).unwrap();
        assert!(
            list_all(&store, &granted, 100).is_err(),
            "{provider:?}: a replaced granted folder is refused"
        );
        assert_eq!(
            fake.runs(),
            1,
            "{provider:?}: refusals ask for no new authorization"
        );
    }
}

#[test]
fn elevated_session_is_closed_to_other_processes() {
    for provider in PROVIDERS {
        let fake = FakeElevation::new(provider, IDLE);
        let backend = fake.backend();
        let root = tempfile::tempdir().unwrap();
        let (store, _session) = open_window(&fake, &backend, root.path());
        list_all(&store, root.path(), 100).unwrap();

        // Only Musheen can write into the session: another process of the
        // user can neither reopen the broker's input nor open its terminal.
        let expected: &[&str] = match provider {
            PrivilegeProvider::Polkit => &["pipe=opened", "input=refused"],
            PrivilegeProvider::Sudo => &["musheen=refused", "unlocked=opened", "broker=refused"],
        };
        assert_eq!(fake.probes(), expected, "{provider:?}");
    }
}

/// Whether process `pid` still runs.
fn running(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(')')
                .map(|(_, rest)| !rest.trim_start().starts_with('Z'))
        })
        .unwrap_or(false)
}

#[test]
fn elevated_session_ends_with_its_window_and_after_idle() {
    assert_eq!(ELEVATED_SESSION_IDLE, Duration::from_secs(15 * 60));
    for provider in PROVIDERS {
        let fake = FakeElevation::new(provider, IDLE);
        let backend = fake.backend();
        let root = tempfile::tempdir().unwrap();
        let (store, session) = open_window(&fake, &backend, root.path());
        list_all(&store, root.path(), 100).unwrap();
        let brokers = fake.broker_pids();
        assert!(!brokers.is_empty(), "{provider:?} started a broker");

        drop(store);
        drop(session);
        drop(backend);
        let closed = std::time::Instant::now();
        while brokers.iter().any(|pid| running(*pid)) && closed.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !brokers.iter().any(|pid| running(*pid)),
            "{provider:?}: the broker stops within 5 seconds of its window closing"
        );

        for index in 0..3 {
            fs::write(root.path().join(format!("{index}.txt")), b"").unwrap();
        }
        let fake = FakeElevation::new(provider, Duration::from_millis(500));
        let backend = fake.backend();
        let (store, _session) = open_window(&fake, &backend, root.path());
        list_all(&store, root.path(), 100).unwrap();
        let location = StorePath::from_unix_path(root.path().as_os_str());
        let first = futures_lite::future::block_on(store.read_directory(
            &location,
            PageRequest::new(2, None).unwrap(),
            CancellationToken::new(),
        ))
        .unwrap();
        std::thread::sleep(Duration::from_millis(1_500));
        let expired = list_all(&store, root.path(), 100);
        assert!(
            expired
                .as_ref()
                .is_err_and(|error| error.to_string().contains("authorization expired")),
            "{provider:?}: an idle broker serves nothing: {expired:?}"
        );
        let continued = futures_lite::future::block_on(store.read_directory(
            &location,
            first.next_request().expect("a second page"),
            CancellationToken::new(),
        ));
        assert!(
            continued
                .as_ref()
                .is_err_and(|error| error.to_string().contains("authorization expired")),
            "{provider:?}: a fetched listing's later pages end with the session: {:?}",
            continued.map(|page| page.items().len())
        );
        assert_eq!(fake.runs(), 1, "{provider:?}");
    }
}

#[test]
fn elevated_session_counts_idle_time_across_a_suspend() {
    for provider in PROVIDERS {
        let fake = FakeElevation::suspending(provider);
        let backend = fake.backend();
        let root = tempfile::tempdir().unwrap();
        let (store, _session) = open_window(&fake, &backend, root.path());
        list_all(&store, root.path(), 100).unwrap();

        // The machine was suspended past the idle limit after that listing.
        let after = list_all(&store, root.path(), 100);
        assert!(
            after
                .as_ref()
                .is_err_and(|error| error.to_string().contains("authorization expired")),
            "{provider:?}: a broker idle across a suspend serves nothing: {after:?}"
        );
        assert_eq!(fake.runs(), 1, "{provider:?}");
    }
}

#[test]
fn elevated_session_keeps_reading_after_a_cancelled_listing() {
    for provider in PROVIDERS {
        let fake = FakeElevation::new(provider, Duration::from_secs(1));
        let backend = fake.backend();
        let root = tempfile::tempdir().unwrap();
        for index in 0..5_000 {
            fs::write(
                root.path().join(format!(
                    "a-file-with-a-long-name-for-the-listing-{index:05}"
                )),
                b"",
            )
            .unwrap();
        }
        let (_store, session) = open_window(&fake, &backend, root.path());
        let brokers = fake.broker_pids();

        // Cancelled once sent: its answer, larger than the socket or terminal
        // buffers, is never asked for.
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let answer = futures_lite::future::block_on(session.read_directory(
            session.root_reference().clone(),
            PathBuf::new(),
            cancelled,
        ));
        assert!(answer.is_err(), "{provider:?}");

        // The broker still delivers the answer, goes idle and ends.
        let cancelled_at = std::time::Instant::now();
        while brokers.iter().any(|pid| running(*pid))
            && cancelled_at.elapsed() < Duration::from_secs(5)
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !brokers.iter().any(|pid| running(*pid)),
            "{provider:?}: a cancelled listing leaves the broker able to go idle"
        );
    }
}

#[test]
fn elevated_session_holds_its_granted_folder() {
    for provider in PROVIDERS {
        let fake = FakeElevation::new(provider, IDLE);
        let backend = fake.backend();
        let root = tempfile::tempdir().unwrap();
        let (store, _session) = open_window(&fake, &backend, root.path());
        list_all(&store, root.path(), 100).unwrap();

        // An open descriptor keeps the folder's inode allocated, so a folder
        // created again at its path cannot take its inode number.
        let held = fake.broker_pids().iter().any(|pid| {
            fs::read_dir(format!("/proc/{pid}/fd"))
                .into_iter()
                .flatten()
                .flatten()
                .any(|descriptor| {
                    fs::read_link(descriptor.path()).is_ok_and(|target| target == root.path())
                })
        });
        assert!(
            held,
            "{provider:?}: the broker holds the granted folder open"
        );
    }
}

#[test]
fn elevated_session_pages_each_listing_from_its_own_snapshot() {
    for provider in PROVIDERS {
        let fake = FakeElevation::new(provider, IDLE);
        let backend = fake.backend();
        let root = tempfile::tempdir().unwrap();
        for folder in ["a", "b"] {
            fs::create_dir(root.path().join(folder)).unwrap();
            for index in 0..25 {
                fs::write(
                    root.path()
                        .join(folder)
                        .join(format!("{folder}-{index:02}")),
                    b"",
                )
                .unwrap();
            }
        }
        let (store, _session) = open_window(&fake, &backend, root.path());

        // Two tabs page through two folders in turn; a file added after
        // their first pages belongs to neither listing.
        let locations = ["a", "b"]
            .map(|folder| StorePath::from_unix_path(root.path().join(folder).into_os_string()));
        let mut requests = [0, 1].map(|_| Some(PageRequest::new(10, None).unwrap()));
        let mut names = [Vec::new(), Vec::new()];
        let mut first_round = true;
        while requests.iter().any(Option::is_some) {
            for ((request, location), names) in
                requests.iter_mut().zip(&locations).zip(names.iter_mut())
            {
                let Some(pending) = request.take() else {
                    continue;
                };
                let page = futures_lite::future::block_on(store.read_directory(
                    location,
                    pending,
                    CancellationToken::new(),
                ))
                .unwrap_or_else(|error| panic!("{provider:?} pages {location:?}: {error}"));
                names.extend(
                    page.items()
                        .iter()
                        .map(|item| item.display_name().as_str().to_owned()),
                );
                *request = page.next_request();
            }
            if first_round {
                fs::write(root.path().join("a").join("a-added"), b"").unwrap();
                first_round = false;
            }
        }
        for (index, folder) in ["a", "b"].into_iter().enumerate() {
            names[index].sort();
            let expected = (0..25)
                .map(|entry| format!("{folder}-{entry:02}"))
                .collect::<Vec<_>>();
            assert_eq!(names[index], expected, "{provider:?}");
        }
        assert_eq!(
            fake.listings(),
            2,
            "{provider:?}: one listing for each folder, whatever the paging order"
        );
    }
}

// SYS-037: owner and group changes as administrator.

/// Opens `root` as administrator and returns why Open as Administrator
/// refused.
fn open_window_error(
    fake: &FakeElevation,
    backend: &Arc<SystemPrivilegeBackend>,
    root: &Path,
) -> BrokerError {
    let request = BrokerRequest::open_directory(root).unwrap();
    match futures_lite::future::block_on(backend.open_window(
        &request,
        CancellationToken::new(),
        fake.password(),
    )) {
        Ok(_) => panic!("Open as Administrator refuses"),
        Err(error) => error,
    }
}

/// Waits up to 5 seconds for every broker `fake` started to stop.
fn brokers_stop(fake: &FakeElevation) -> bool {
    let started = std::time::Instant::now();
    while fake.broker_pids().iter().any(|pid| running(*pid))
        && started.elapsed() < Duration::from_secs(5)
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    !fake.broker_pids().iter().any(|pid| running(*pid))
}

#[test]
fn elevated_session_reads_the_installed_brokers_version_before_authorization() {
    for provider in PROVIDERS {
        let fake = FakeElevation::with_broker_environment(
            provider,
            &format!("{BROKER_INSTALLED_PROTOCOL}='999'"),
        );
        let backend = fake.backend();
        let root = tempfile::tempdir().unwrap();
        let error = open_window_error(&fake, &backend, root.path());
        assert!(
            error.to_string().contains("restart Musheen"),
            "{provider:?}: {error}"
        );
        assert_eq!(
            fake.runs(),
            0,
            "{provider:?}: no authorization is asked for"
        );
        assert!(
            fake.broker_pids().is_empty(),
            "{provider:?}: no broker starts"
        );
    }
}

#[test]
fn elevated_session_refuses_a_broker_that_answers_with_another_version() {
    for provider in PROVIDERS {
        let fake = FakeElevation::with_broker_environment(
            provider,
            &format!("{BROKER_ANSWER_PROTOCOL}='999'"),
        );
        let backend = fake.backend();
        let root = tempfile::tempdir().unwrap();
        let error = open_window_error(&fake, &backend, root.path());
        assert!(
            error.to_string().contains("restart Musheen"),
            "{provider:?}: {error}"
        );
        assert!(
            brokers_stop(&fake),
            "{provider:?}: the broker stops within 5 seconds of the mismatch"
        );
    }
}

#[test]
fn elevated_session_ends_when_an_answer_cannot_be_read() {
    for provider in PROVIDERS {
        let fake =
            FakeElevation::with_broker_environment(provider, &format!("{BROKER_GARBLE}='1'"));
        let backend = fake.backend();
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("notes.txt"), b"notes").unwrap();
        let (store, _session) = open_window(&fake, &backend, root.path());
        let error = list_all(&store, root.path(), 100)
            .expect_err("an answer that cannot be decoded fails the listing");
        assert!(
            error.to_string().contains("session ended"),
            "{provider:?}: the window says the session ended: {error}"
        );
        assert!(
            brokers_stop(&fake),
            "{provider:?}: the broker stops within 5 seconds"
        );
        assert_eq!(fake.runs(), 1, "{provider:?}");
    }
}

#[test]
fn change_ownership_policy_always_asks_for_an_administrator_password() {
    let policy = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packaging/polkit/org.musheen.Musheen.policy"
    ))
    .unwrap();
    let action = policy
        .split("<action id=\"org.musheen.change-ownership-as-administrator\">")
        .nth(1)
        .expect("the policy declares the change-ownership action");
    let action = &action[..action.find("</action>").unwrap()];
    for rule in [
        "<allow_any>no</allow_any>",
        "<allow_inactive>no</allow_inactive>",
        "<allow_active>auth_admin</allow_active>",
    ] {
        assert!(action.contains(rule), "the action has {rule}");
    }
    assert!(
        musheen_desktop::privilege::ADMIN_ACTION_IDS
            .contains(&"org.musheen.change-ownership-as-administrator")
    );
}

// The broker's own ownership change, run as root in a user namespace that
// maps this user's subordinate IDs, so owners really change without root on
// the machine: `unshare --map-auto --map-root-user --mount` starts this test
// binary as `change_ownership_scenario_child`, which runs one scenario and
// checks it from inside. Where user namespaces are not allowed, the tests
// say so and pass.

const OWNERSHIP_SCENARIO: &str = "MUSHEEN_OWNERSHIP_SCENARIO";
const OWNERSHIP_ROOT: &str = "MUSHEEN_OWNERSHIP_ROOT";

/// Runs `scenario` in its own user and mount namespace over the folder
/// `root`, and fails when it fails.
fn run_ownership_scenario(scenario: &str) {
    let allowed = std::process::Command::new("unshare")
        .args(["--map-auto", "--map-root-user", "true"])
        .status()
        .is_ok_and(|status| status.success());
    if !allowed {
        eprintln!("{scenario}: user namespaces are not allowed here; not checked");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new("unshare")
        .args(["--map-auto", "--map-root-user", "--mount"])
        .arg(std::env::current_exe().unwrap())
        .args([
            "change_ownership_scenario_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(OWNERSHIP_SCENARIO, scenario)
        .env(OWNERSHIP_ROOT, root.path())
        .output()
        .unwrap();
    // The scenario gave files to other owners, which this user cannot
    // remove; inside a namespace again, every one belongs to this user.
    let _ = std::process::Command::new("unshare")
        .args(["--map-auto", "--map-root-user", "rm", "-rf", "--"])
        .arg(root.path())
        .status();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{scenario} failed inside the namespace:\n{stdout}\n{stderr}"
    );
}

/// Counts authorizations and grants each, as pkexec does once it
/// authenticated.
#[derive(Clone, Default)]
struct CountingAuthorizer(Arc<std::sync::atomic::AtomicUsize>);

impl Authorizer for CountingAuthorizer {
    fn authorize(
        &self,
        request: &AuthorizationRequest,
    ) -> Result<AuthorizationGrant, AuthorizationError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        AllowInvoker.authorize(request)
    }
}

fn change_ownership(
    authorizer: &CountingAuthorizer,
    items: Vec<OwnershipItem>,
    owner: Option<u32>,
    group: Option<u32>,
    contents: Option<OwnershipContents>,
) -> Result<OwnershipReport, BrokerError> {
    let request = BrokerRequest::change_ownership(items, owner, group, contents)?;
    let broker = Broker::new(
        authorizer.clone(),
        SystemOperationRunner::default(),
        NoopAudit,
        SystemClock,
    );
    match broker.handle(request)? {
        BrokerOutput::OwnershipChanged(report) => Ok(report),
        other => panic!("an ownership change answered {other:?}"),
    }
}

fn reviewed(path: &Path) -> OwnershipItem {
    OwnershipItem::reviewed(path).unwrap()
}

/// The owner, group and mode of `path` itself, a link as the link.
fn ownership(path: &Path) -> (u32, u32, u32) {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.uid(), metadata.gid(), metadata.mode() & 0o7777)
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn mount(arguments: &[&str]) {
    let status = std::process::Command::new("mount")
        .args(arguments)
        .status()
        .unwrap();
    assert!(status.success(), "mount {arguments:?}");
}

#[test]
fn change_ownership_scenario_child() {
    let (Ok(scenario), Ok(root)) = (
        std::env::var(OWNERSHIP_SCENARIO),
        std::env::var(OWNERSHIP_ROOT),
    ) else {
        return;
    };
    let root = PathBuf::from(root);
    let authorizer = CountingAuthorizer::default();
    let authorizations = || authorizer.0.load(std::sync::atomic::Ordering::SeqCst);
    match scenario.as_str() {
        "owners" => {
            let (file, other, folder) = (root.join("a"), root.join("b"), root.join("d"));
            fs::write(&file, b"a").unwrap();
            fs::write(&other, b"b").unwrap();
            fs::create_dir(&folder).unwrap();
            fs::write(folder.join("x"), b"x").unwrap();
            fs::create_dir(folder.join("y")).unwrap();
            set_mode(&file, 0o640);
            set_mode(&folder, 0o750);
            set_mode(&folder.join("x"), 0o600);
            let report = change_ownership(
                &authorizer,
                vec![reviewed(&file), reviewed(&folder)],
                Some(1000),
                Some(1001),
                Some(OwnershipContents {
                    nested_mounts: false,
                }),
            )
            .unwrap();
            assert_eq!(report.failure(), None);
            assert_eq!(report.changed(), 4, "a, d, d/x and d/y");
            assert_eq!(
                authorizations(),
                1,
                "one authorization for the whole change"
            );
            assert_eq!(ownership(&file), (1000, 1001, 0o640), "no mode changes");
            assert_eq!(ownership(&folder), (1000, 1001, 0o750));
            assert_eq!(ownership(&folder.join("x")), (1000, 1001, 0o600));
            assert_eq!(ownership(&folder.join("y")).0, 1000);
            assert_eq!(ownership(&other).0, 0, "an item not named stays as it is");
            let group_only =
                change_ownership(&authorizer, vec![reviewed(&other)], None, Some(1002), None)
                    .unwrap();
            assert_eq!(group_only.changed(), 1);
            assert_eq!(ownership(&other).0, 0, "no owner asked, none changed");
            assert_eq!(ownership(&other).1, 1002);
        }
        "links" => {
            let outside = root.join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("o"), b"o").unwrap();
            let link = root.join("l");
            std::os::unix::fs::symlink(outside.join("o"), &link).unwrap();
            let folder = root.join("d");
            fs::create_dir(&folder).unwrap();
            std::os::unix::fs::symlink(&outside, folder.join("m")).unwrap();
            let report = change_ownership(
                &authorizer,
                vec![reviewed(&link), reviewed(&folder)],
                Some(1000),
                None,
                Some(OwnershipContents {
                    nested_mounts: false,
                }),
            )
            .unwrap();
            assert_eq!(report.failure(), None);
            assert_eq!(report.changed(), 3, "l, d and d/m, each itself");
            assert_eq!(ownership(&link).0, 1000, "the link itself changed");
            assert_eq!(ownership(&folder.join("m")).0, 1000);
            assert_eq!(ownership(&outside.join("o")).0, 0, "never a link's target");
            assert_eq!(ownership(&outside).0, 0);
        }
        "replaced" => {
            let file = root.join("f");
            fs::write(&file, b"reviewed").unwrap();
            let item = reviewed(&file);
            fs::write(root.join("new"), b"new").unwrap();
            fs::rename(root.join("new"), &file).unwrap();
            let refused =
                change_ownership(&authorizer, vec![item], Some(1000), None, None).unwrap();
            assert_eq!(refused.changed(), 0);
            let failure = refused.failure().expect("a replaced item is refused");
            assert_eq!(failure.path(), file, "the refusal names its item");
            assert_eq!(failure.error(), BrokerError::TargetReplaced);
            assert_eq!(ownership(&file).0, 0);
            assert_eq!(authorizations(), 0, "refused before it asks");

            // With several items, the one replaced is named, and none changes.
            let (first, second) = (root.join("h1"), root.join("h2"));
            fs::write(&first, b"1").unwrap();
            fs::write(&second, b"2").unwrap();
            let items = vec![reviewed(&first), reviewed(&second)];
            fs::write(root.join("h2.new"), b"new").unwrap();
            fs::rename(root.join("h2.new"), &second).unwrap();
            let refused = change_ownership(&authorizer, items, Some(1000), None, None).unwrap();
            assert_eq!(refused.changed(), 0);
            assert_eq!(
                refused.failure().map(|failure| failure.path()),
                Some(second.as_path())
            );
            assert_eq!(
                ownership(&first).0,
                0,
                "nothing changes before every item is checked"
            );
            assert_eq!(authorizations(), 0);

            // Replaced while authorization was asked.
            let second = root.join("g");
            fs::write(&second, b"reviewed").unwrap();
            let request =
                BrokerRequest::change_ownership(vec![reviewed(&second)], Some(1000), None, None)
                    .unwrap();
            struct Swapping(PathBuf);
            impl Authorizer for Swapping {
                fn authorize(
                    &self,
                    request: &AuthorizationRequest,
                ) -> Result<AuthorizationGrant, AuthorizationError> {
                    let spare = self.0.with_extension("spare");
                    fs::write(&spare, b"swapped").unwrap();
                    fs::rename(&spare, &self.0).unwrap();
                    AllowInvoker.authorize(request)
                }
            }
            let broker = Broker::new(
                Swapping(second.clone()),
                SystemOperationRunner::default(),
                NoopAudit,
                SystemClock,
            );
            match broker.handle(request) {
                Ok(BrokerOutput::OwnershipChanged(report)) => {
                    let failure = report.failure().expect("the swapped item is refused");
                    assert_eq!(failure.path(), second);
                    assert_eq!(failure.error(), BrokerError::TargetReplaced);
                }
                other => panic!("a swapped item answered {other:?}"),
            }
            assert_eq!(ownership(&second).0, 0);
        }
        "scope" => {
            let folder = root.join("d");
            fs::create_dir(&folder).unwrap();
            fs::write(folder.join("x"), b"x").unwrap();
            let mounted = folder.join("mnt");
            fs::create_dir(&mounted).unwrap();
            mount(&["-t", "tmpfs", "none", mounted.to_str().unwrap()]);
            fs::write(mounted.join("z"), b"z").unwrap();
            // A bind mount of the same filesystem keeps its folder's device;
            // it is still a nested mount.
            let elsewhere = root.join("elsewhere");
            fs::create_dir(&elsewhere).unwrap();
            fs::write(elsewhere.join("w"), b"w").unwrap();
            let bound = folder.join("bound");
            fs::create_dir(&bound).unwrap();
            mount(&[
                "--bind",
                elsewhere.to_str().unwrap(),
                bound.to_str().unwrap(),
            ]);

            let alone =
                change_ownership(&authorizer, vec![reviewed(&folder)], Some(1000), None, None)
                    .unwrap();
            assert_eq!(
                alone.changed(),
                1,
                "without Apply to contents, only the folder"
            );
            assert_eq!(ownership(&folder.join("x")).0, 0);

            let without_mounts = change_ownership(
                &authorizer,
                vec![reviewed(&folder)],
                Some(1001),
                None,
                Some(OwnershipContents {
                    nested_mounts: false,
                }),
            )
            .unwrap();
            assert_eq!(
                without_mounts.changed(),
                2,
                "d and d/x; not the nested mounts"
            );
            assert_eq!(ownership(&folder.join("x")).0, 1001);
            assert_eq!(ownership(&mounted).0, 0, "the tmpfs is left out");
            assert_eq!(ownership(&mounted.join("z")).0, 0);
            assert_eq!(ownership(&bound).0, 0, "the bind mount is left out");
            assert_eq!(ownership(&elsewhere.join("w")).0, 0);

            let with_mounts = change_ownership(
                &authorizer,
                vec![reviewed(&folder)],
                Some(1002),
                None,
                Some(OwnershipContents {
                    nested_mounts: true,
                }),
            )
            .unwrap();
            assert_eq!(with_mounts.changed(), 6, "d, x, mnt, z, bound and w");
            assert_eq!(ownership(&mounted.join("z")).0, 1002);
            assert_eq!(ownership(&elsewhere.join("w")).0, 1002);
        }
        "unchanged" => {
            // Any chown would clear a setuid bit, so an item or entry that
            // already has the owner and group asked for is not touched.
            let file = root.join("s");
            fs::write(&file, b"s").unwrap();
            set_mode(&file, 0o4755);
            let folder = root.join("d");
            fs::create_dir(&folder).unwrap();
            let inner = folder.join("t");
            fs::write(&inner, b"t").unwrap();
            set_mode(&inner, 0o4755);
            let other = folder.join("u");
            fs::write(&other, b"u").unwrap();
            std::os::unix::fs::chown(&other, Some(1000), Some(1000)).unwrap();
            let report = change_ownership(
                &authorizer,
                vec![reviewed(&file), reviewed(&folder)],
                Some(0),
                Some(0),
                Some(OwnershipContents {
                    nested_mounts: false,
                }),
            )
            .unwrap();
            assert_eq!(report.failure(), None);
            assert_eq!(report.changed(), 1, "only d/u needed a change");
            assert_eq!(
                ownership(&file),
                (0, 0, 0o4755),
                "a matching item keeps setuid"
            );
            assert_eq!(
                ownership(&inner),
                (0, 0, 0o4755),
                "a matching entry keeps setuid"
            );
            assert_eq!(ownership(&other).0, 0);
        }
        "special" => {
            let folder = root.join("d");
            fs::create_dir(&folder).unwrap();
            fs::write(folder.join("f"), b"f").unwrap();
            let pipe = folder.join("p");
            let made = std::process::Command::new("mkfifo")
                .arg(&pipe)
                .status()
                .unwrap();
            assert!(made.success());
            let socket = folder.join("s");
            let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let report = change_ownership(
                &authorizer,
                vec![reviewed(&folder), reviewed(&pipe)],
                Some(1000),
                None,
                Some(OwnershipContents {
                    nested_mounts: false,
                }),
            )
            .unwrap();
            assert_eq!(report.failure(), None);
            assert_eq!(report.changed(), 2, "d and d/f");
            assert_eq!(ownership(&folder.join("f")).0, 1000);
            assert_eq!(
                ownership(&pipe).0,
                0,
                "a pipe, named or inside, is left as it is"
            );
            assert_eq!(ownership(&socket).0, 0, "a socket is left as it is");
        }
        "progress" => {
            let folder = root.join("d");
            fs::create_dir(&folder).unwrap();
            for name in ["a", "b", "c"] {
                fs::write(folder.join(name), name).unwrap();
            }
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let recorded = Arc::clone(&seen);
            let runner = SystemOperationRunner::default().with_progress(Arc::new(
                move |changed, path: &Path| {
                    recorded.lock().unwrap().push((changed, path.to_path_buf()));
                },
            ));
            let broker = Broker::new(authorizer.clone(), runner, NoopAudit, SystemClock);
            let request = BrokerRequest::change_ownership(
                vec![reviewed(&folder)],
                Some(1000),
                None,
                Some(OwnershipContents {
                    nested_mounts: false,
                }),
            )
            .unwrap();
            match broker.handle(request) {
                Ok(BrokerOutput::OwnershipChanged(report)) => assert_eq!(report.changed(), 4),
                other => panic!("an ownership change answered {other:?}"),
            }
            let seen = seen.lock().unwrap();
            assert_eq!(
                seen.first(),
                Some(&(1, folder.clone())),
                "the first item reports at once"
            );
        }
        "failure" => {
            let file = root.join("a");
            fs::write(&file, b"a").unwrap();
            let locked = root.join("locked");
            fs::create_dir(&locked).unwrap();
            let stuck = locked.join("b");
            fs::write(&stuck, b"b").unwrap();
            let after = root.join("c");
            fs::write(&after, b"c").unwrap();
            let items = vec![reviewed(&file), reviewed(&stuck), reviewed(&after)];
            let locked_path = locked.to_str().unwrap();
            mount(&["--bind", locked_path, locked_path]);
            mount(&["-o", "remount,bind,ro", locked_path]);
            let report = change_ownership(&authorizer, items, Some(1000), None, None).unwrap();
            assert_eq!(report.changed(), 1, "the item before the failure changed");
            let failure = report.failure().expect("a read-only item fails");
            assert_eq!(failure.path(), stuck, "the failure names its item");
            assert_eq!(
                ownership(&file).0,
                1000,
                "items already changed stay changed"
            );
            assert_eq!(ownership(&after).0, 0, "the change stops at the failure");
        }
        other => panic!("unknown ownership scenario {other}"),
    }
}

#[test]
fn change_ownership_changes_owners_and_groups_after_one_authorization() {
    run_ownership_scenario("owners");
}

#[test]
fn change_ownership_changes_a_link_itself_and_never_its_target() {
    run_ownership_scenario("links");
}

#[test]
fn change_ownership_refuses_an_item_replaced_since_the_review() {
    run_ownership_scenario("replaced");
}

#[test]
fn change_ownership_stays_within_the_reviewed_scope() {
    run_ownership_scenario("scope");
}

#[test]
fn change_ownership_names_the_item_that_failed() {
    run_ownership_scenario("failure");
}

#[test]
fn change_ownership_leaves_items_that_match_as_they_are() {
    run_ownership_scenario("unchanged");
}

#[test]
fn change_ownership_leaves_sockets_pipes_and_devices_as_they_are() {
    run_ownership_scenario("special");
}

#[test]
fn change_ownership_reports_its_progress() {
    run_ownership_scenario("progress");
}
