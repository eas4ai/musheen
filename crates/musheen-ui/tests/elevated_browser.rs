use musheen_core::{CancellationToken, PageRequest, ResourceLimits, Store, StorePath};
use musheen_desktop::SecretBuffer;
use musheen_desktop::privilege::{
    AuthorizationError, AuthorizationGrant, AuthorizationRequest, Authorizer, Broker, BrokerLaunch,
    BrokerOutput, BrokerRequest, BrokerResponse, BrokerTransport, ELEVATED_SESSION_IDLE,
    ElevatedRootReference, NoopAudit, ProcessBrokerTransport, RequestLines, SUDO_BROKER_READY,
    SudoPtyBrokerTransport, SystemClock, SystemOperationRunner, decode_broker_request,
    prepare_sudo_terminal, serve_session, write_response,
};
use musheen_desktop::{Clock, PrivilegeProvider, RootGrant, RootedStore};
use musheen_ui::{
    AppearanceMode, Catalog, ElevatedBrowser, ElevatedSession, Locale, PrivilegeBackend,
    RootedFilesystemStore, SystemPrivilegeBackend,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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

/// Plays musheen-broker when a fake pkexec or sudo starts this test binary,
/// and does nothing otherwise. It runs the broker's own session code, with an
/// authorizer that allows the invoking user.
#[test]
fn elevated_session_broker_child() {
    use std::io::Write as _;

    let Ok(arguments) = std::env::var(BROKER_ARGUMENTS) else {
        return;
    };
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
    if provider == PrivilegeProvider::Sudo {
        prepare_sudo_terminal().unwrap();
        println!("{SUDO_BROKER_READY}");
    }
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
    if let Ok(BrokerOutput::RootReferenced(root)) = opened {
        serve_session(&broker, &root, &requests, &mut output, &bind, idle);
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
        let directory = tempfile::tempdir().unwrap();
        let runs = directory.path().join("runs");
        let pids = directory.path().join("pids");
        let broker = executable_script(
            directory.path(),
            "broker",
            &format!(
                "{BROKER_ARGUMENTS}=\"$*\" {BROKER_PIDS}='{}' {BROKER_IDLE_MILLIS}='{}' '{}' \
                 elevated_session_broker_child --exact --nocapture --test-threads=1 \
                 | /usr/bin/grep --line-buffered -o 'MUSHEEN_.*'",
                pids.display(),
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
        let names = list_all(&store, root.path(), 1_000)
            .unwrap_or_else(|error| panic!("{provider:?} lists 2,000 entries: {error}"));
        assert_eq!(names.len(), 2_000, "{provider:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{provider:?} took {:?}",
            started.elapsed()
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

        let fake = FakeElevation::new(provider, Duration::from_millis(500));
        let backend = fake.backend();
        let (store, _session) = open_window(&fake, &backend, root.path());
        list_all(&store, root.path(), 100).unwrap();
        std::thread::sleep(Duration::from_millis(1_500));
        let expired = list_all(&store, root.path(), 100);
        assert!(
            expired
                .as_ref()
                .is_err_and(|error| error.to_string().contains("authorization expired")),
            "{provider:?}: an idle broker serves nothing: {expired:?}"
        );
        assert_eq!(fake.runs(), 1, "{provider:?}");
    }
}
