mod support;

use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::{
    MetadataFetcher, UpdateCheck, UpdateError, UpdateMetadataVerifier, UpdateOffer, UpdatePolicy,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use support::UsableFileManager;

struct FakeFetcher {
    body: Box<[u8]>,
    calls: Arc<AtomicUsize>,
}

impl MetadataFetcher for FakeFetcher {
    fn fetch(
        &self,
        _url: &str,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<Box<[u8]>, UpdateError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let body = self.body.clone();
        Box::pin(async move { Ok(body) })
    }
}

#[test]
fn disabled_and_not_yet_due_checks_do_not_fetch() {
    let calls = Arc::new(AtomicUsize::new(0));
    let check = UpdateCheck::new(
        FakeFetcher {
            body: Box::default(),
            calls: Arc::clone(&calls),
        },
        UpdateMetadataVerifier::project_key(),
        "0.1.0",
    );

    let disabled = futures_lite::future::block_on(check.check(
        UpdatePolicy::disabled(),
        100,
        CancellationToken::new(),
    ));
    let delayed = futures_lite::future::block_on(check.check(
        UpdatePolicy::enabled("https://updates.musheen.test/latest.json", 200),
        100,
        CancellationToken::new(),
    ));

    assert_eq!(disabled.unwrap(), UpdateOffer::Disabled);
    assert_eq!(delayed.unwrap(), UpdateOffer::NotDue);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn rejects_non_https_metadata_before_fetch() {
    let calls = Arc::new(AtomicUsize::new(0));
    let check = UpdateCheck::new(
        FakeFetcher {
            body: Box::default(),
            calls: Arc::clone(&calls),
        },
        UpdateMetadataVerifier::project_key(),
        "0.1.0",
    );
    let result = futures_lite::future::block_on(check.check(
        UpdatePolicy::enabled("http://updates.musheen.test/latest.json", 0),
        100,
        CancellationToken::new(),
    ));
    assert!(matches!(result, Err(UpdateError::InsecureTransport)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn only_versions_newer_than_the_running_application_are_offered() {
    let body: Box<[u8]> = include_bytes!("fixtures/update-valid.json")
        .as_slice()
        .into();
    for (running, expected) in [("0.1.0", true), ("0.2.0", false), ("0.3.0", false)] {
        let check = UpdateCheck::new(
            FakeFetcher {
                body: body.clone(),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            UpdateMetadataVerifier::project_key(),
            running,
        );
        let offer = futures_lite::future::block_on(check.check(
            UpdatePolicy::enabled("https://updates.musheen.test/latest.json", 0),
            2_000_000_000,
            CancellationToken::new(),
        ))
        .unwrap();
        assert_eq!(matches!(offer, UpdateOffer::Information(_)), expected);
    }
}

#[test]
fn signed_metadata_validates_tampering_expiry_and_information_only_offer() {
    let verifier = UpdateMetadataVerifier::project_key();
    let valid = include_bytes!("fixtures/update-valid.json");
    let tampered = include_bytes!("fixtures/update-tampered.json");
    let expired = include_bytes!("fixtures/update-expired.json");

    assert!(verifier.verify(tampered, 2_000_000_000).is_err());
    assert!(matches!(
        verifier.verify(expired, 2_000_000_000),
        Err(UpdateError::Expired)
    ));
    let offer = verifier.verify(valid, 2_000_000_000).unwrap();
    assert_eq!(offer.version(), "0.2.0");
    assert_eq!(
        offer.information_url(),
        "https://musheen.test/releases/0.2.0"
    );
    assert!(!offer.can_install());
}

#[derive(Clone, Copy)]
enum UpdateServiceMode {
    Absent,
    Slow,
    Disconnected,
    Restarted,
}

#[derive(Clone)]
struct MatrixFetcher {
    mode: Arc<Mutex<UpdateServiceMode>>,
    started: async_channel::Sender<()>,
    release: async_channel::Receiver<()>,
}

impl MetadataFetcher for MatrixFetcher {
    fn fetch(
        &self,
        _url: &str,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<Box<[u8]>, UpdateError>> {
        let mode = *self.mode.lock().unwrap();
        let started = self.started.clone();
        let release = self.release.clone();
        Box::pin(async move {
            match mode {
                UpdateServiceMode::Absent => Err(UpdateError::Transport("absent".into())),
                UpdateServiceMode::Disconnected => {
                    Err(UpdateError::Transport("disconnected".into()))
                }
                UpdateServiceMode::Slow => {
                    started.send(()).await.unwrap();
                    release.recv().await.unwrap();
                    Err(UpdateError::Transport("slow".into()))
                }
                UpdateServiceMode::Restarted => Ok(include_bytes!("fixtures/update-valid.json")
                    .as_slice()
                    .into()),
            }
        })
    }
}

#[test]
fn update_absence_slowness_disconnect_and_restart_leave_file_management_usable() {
    let (started, started_rx) = async_channel::bounded(1);
    let (release, release_rx) = async_channel::bounded(1);
    let mode = Arc::new(Mutex::new(UpdateServiceMode::Absent));
    let fetcher = MatrixFetcher {
        mode: Arc::clone(&mode),
        started,
        release: release_rx,
    };
    let file_manager = UsableFileManager::new();
    let check = |fetcher| UpdateCheck::new(fetcher, UpdateMetadataVerifier::project_key(), "0.1.0");
    let policy = || UpdatePolicy::enabled("https://updates.musheen.test/latest.json", 0);

    for service_mode in [UpdateServiceMode::Absent, UpdateServiceMode::Disconnected] {
        *mode.lock().unwrap() = service_mode;
        let result = futures_lite::future::block_on(check(fetcher.clone()).check(
            policy(),
            2_000_000_000,
            CancellationToken::new(),
        ));
        assert!(matches!(result, Err(UpdateError::Transport(_))));
        file_manager.show("usable");
    }

    *mode.lock().unwrap() = UpdateServiceMode::Slow;
    let slow_fetcher = fetcher.clone();
    let slow = std::thread::spawn(move || {
        futures_lite::future::block_on(check(slow_fetcher).check(
            policy(),
            2_000_000_000,
            CancellationToken::new(),
        ))
    });
    started_rx.recv_blocking().unwrap();
    file_manager.show("still-usable");
    release.send_blocking(()).unwrap();
    assert!(matches!(
        slow.join().unwrap(),
        Err(UpdateError::Transport(_))
    ));

    *mode.lock().unwrap() = UpdateServiceMode::Restarted;
    let offer = futures_lite::future::block_on(check(fetcher).check(
        policy(),
        2_000_000_000,
        CancellationToken::new(),
    ))
    .unwrap();
    assert!(matches!(offer, UpdateOffer::Information(_)));
    file_manager.show("restarted");
    assert_eq!(file_manager.calls(), 4);
}
