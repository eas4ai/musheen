mod support;

use base64::Engine as _;
use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::{
    MaintenanceTask, MetadataFetcher, UpdateCheck, UpdateError, UpdateInformation,
    UpdateMaintenanceTask, UpdateMetadataVerifier, UpdateOffer, UpdateOfferSink, UpdatePolicy,
    UpdateSequenceStore,
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::{fs, os::unix::fs::PermissionsExt};
use support::UsableFileManager;

const TEST_SEED: [u8; 32] = [7; 32];

fn test_verifier() -> UpdateMetadataVerifier {
    let key = Ed25519KeyPair::from_seed_unchecked(&TEST_SEED).unwrap();
    UpdateMetadataVerifier::from_key(key.public_key().as_ref().try_into().unwrap())
}

fn signed_update(channel: &str, sequence: u64, version: &str, expires_unix: u64) -> Box<[u8]> {
    let key = Ed25519KeyPair::from_seed_unchecked(&TEST_SEED).unwrap();
    let information_url = format!("https://musheen.test/releases/{version}");
    let message = serde_json::to_vec(&(
        2,
        channel,
        sequence,
        version,
        &information_url,
        expires_unix,
    ))
    .unwrap();
    let signature = base64::engine::general_purpose::STANDARD.encode(key.sign(&message).as_ref());
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 2,
        "channel": channel,
        "sequence": sequence,
        "version": version,
        "information_url": information_url,
        "expires_unix": expires_unix,
        "signature": signature,
    }))
    .unwrap()
    .into_boxed_slice()
}

#[test]
fn signed_v2_update_metadata_is_accepted() {
    let document = signed_update("stable", 1, "0.2.0", 2_000_000_001);
    let offer = test_verifier().verify(&document, 2_000_000_000).unwrap();
    assert_eq!(offer.version(), "0.2.0");
}

#[test]
fn signed_metadata_for_another_channel_is_not_offered() {
    let temporary = tempfile::tempdir().unwrap();
    let check = UpdateCheck::with_sequence_store(
        FakeFetcher {
            body: signed_update("beta", 1, "0.2.0", 2_000_000_001),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        test_verifier(),
        "0.1.0",
        UpdateSequenceStore::at(temporary.path().join("updates.json")),
    );
    let result = futures_lite::future::block_on(check.check(
        UpdatePolicy::enabled("https://updates.musheen.test/stable.json", 0),
        2_000_000_000,
        CancellationToken::new(),
    ));

    assert!(result.is_err());
}

#[test]
fn identical_signed_sequence_can_be_reoffered_after_delivery_failure() {
    let temporary = tempfile::tempdir().unwrap();
    let check = UpdateCheck::with_sequence_store(
        FakeFetcher {
            body: signed_update("stable", 4, "0.2.0", 2_000_000_001),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        test_verifier(),
        "0.1.0",
        UpdateSequenceStore::at(temporary.path().join("updates.json")),
    );
    let policy = || UpdatePolicy::enabled("https://updates.musheen.test/stable.json", 0);
    let first = futures_lite::future::block_on(check.check(
        policy(),
        2_000_000_000,
        CancellationToken::new(),
    ));
    let repeated = futures_lite::future::block_on(check.check(
        policy(),
        2_000_000_000,
        CancellationToken::new(),
    ));

    assert!(matches!(first, Ok(UpdateOffer::Information(_))));
    assert!(matches!(repeated, Ok(UpdateOffer::Information(_))));
}

#[test]
fn signed_update_sequence_survives_a_new_checker_and_rejects_rollback() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("updates.json");
    let check = |sequence| {
        UpdateCheck::with_sequence_store(
            FakeFetcher {
                body: signed_update("stable", sequence, "0.2.0", 2_000_000_001),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            test_verifier(),
            "0.1.0",
            UpdateSequenceStore::at(&path),
        )
    };
    let policy = || UpdatePolicy::enabled("https://updates.musheen.test/stable.json", 0);
    let run = |sequence| {
        futures_lite::future::block_on(check(sequence).check(
            policy(),
            2_000_000_000,
            CancellationToken::new(),
        ))
    };

    assert!(matches!(run(4), Ok(UpdateOffer::Information(_))));
    assert!(matches!(run(4), Ok(UpdateOffer::Information(_))));
    assert!(matches!(run(3), Err(UpdateError::Replay)));
    assert!(matches!(run(5), Ok(UpdateOffer::Information(_))));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn damaged_sequence_state_fails_closed_without_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("updates.json");
    let damaged = br#"{"schema_version":2,"channels":{"stable":99}}"#;
    fs::write(&path, damaged).unwrap();
    let check = UpdateCheck::with_sequence_store(
        FakeFetcher {
            body: signed_update("stable", 100, "0.2.0", 2_000_000_001),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        test_verifier(),
        "0.1.0",
        UpdateSequenceStore::at(&path),
    );

    let result = futures_lite::future::block_on(check.check(
        UpdatePolicy::enabled("https://updates.musheen.test/stable.json", 0),
        2_000_000_000,
        CancellationToken::new(),
    ));
    assert!(matches!(result, Err(UpdateError::InvalidState)));
    assert_eq!(fs::read(&path).unwrap(), damaged);
}

#[derive(Clone)]
struct FailingOnceSink {
    calls: Arc<AtomicUsize>,
}

impl UpdateOfferSink for FailingOnceSink {
    fn offer(&self, _offer: UpdateInformation) -> Result<(), Box<str>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("receiver unavailable".into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn failed_offer_delivery_can_retry_the_same_signed_sequence() {
    let temporary = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let task = UpdateMaintenanceTask::new(
        UpdateCheck::with_sequence_store(
            FakeFetcher {
                body: signed_update("stable", 4, "0.2.0", 2_000_000_001),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            test_verifier(),
            "0.1.0",
            UpdateSequenceStore::at(temporary.path().join("updates.json")),
        ),
        UpdatePolicy::enabled("https://updates.musheen.test/stable.json", 0),
        FailingOnceSink {
            calls: Arc::clone(&calls),
        },
    );

    assert!(futures_lite::future::block_on(task.run(CancellationToken::new())).is_err());
    assert!(futures_lite::future::block_on(task.run(CancellationToken::new())).is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[derive(Clone)]
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
    let temporary = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let check = UpdateCheck::with_sequence_store(
        FakeFetcher {
            body: Box::default(),
            calls: Arc::clone(&calls),
        },
        UpdateMetadataVerifier::project_key(),
        "0.1.0",
        UpdateSequenceStore::at(temporary.path().join("updates.json")),
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
    let body = signed_update("stable", 1, "0.2.0", 2_000_000_001);
    for (running, expected) in [("0.1.0", true), ("0.2.0", false), ("0.3.0", false)] {
        let temporary = tempfile::tempdir().unwrap();
        let check = UpdateCheck::with_sequence_store(
            FakeFetcher {
                body: body.clone(),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            test_verifier(),
            running,
            UpdateSequenceStore::at(temporary.path().join("updates.json")),
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
    let verifier = test_verifier();
    let valid = signed_update("stable", 1, "0.2.0", 2_000_000_001);
    let mut tampered: serde_json::Value = serde_json::from_slice(&valid).unwrap();
    tampered["version"] = serde_json::json!("0.3.0");
    let tampered = serde_json::to_vec(&tampered).unwrap();
    let expired = signed_update("stable", 2, "0.2.0", 1_999_999_999);

    assert!(matches!(
        verifier.verify(&tampered, 2_000_000_000),
        Err(UpdateError::InvalidSignature)
    ));
    assert!(matches!(
        verifier.verify(&expired, 2_000_000_000),
        Err(UpdateError::Expired)
    ));
    let offer = verifier.verify(&valid, 2_000_000_000).unwrap();
    assert_eq!(offer.channel(), "stable");
    assert_eq!(offer.sequence(), 1);
    assert_eq!(offer.version(), "0.2.0");
    assert_eq!(
        offer.information_url(),
        "https://musheen.test/releases/0.2.0"
    );
    assert!(!offer.can_install());
}

#[test]
fn legacy_unsigned_and_channel_tampered_metadata_are_not_valid_updates() {
    let verifier = test_verifier();
    let legacy = include_bytes!("fixtures/update-valid.json");
    let unsigned = br#"{"schema_version":2,"channel":"stable","sequence":1,"version":"0.2.0","information_url":"https://musheen.test/releases/0.2.0","expires_unix":2000000001}"#;
    let mut altered: serde_json::Value =
        serde_json::from_slice(&signed_update("stable", 1, "0.2.0", 2_000_000_001)).unwrap();
    altered["channel"] = serde_json::json!("beta");
    let altered = serde_json::to_vec(&altered).unwrap();

    assert!(matches!(
        verifier.verify(legacy, 2_000_000_000),
        Err(UpdateError::InvalidMetadata)
    ));
    assert!(matches!(
        verifier.verify(unsigned, 2_000_000_000),
        Err(UpdateError::InvalidMetadata)
    ));
    assert!(matches!(
        verifier.verify(&altered, 2_000_000_000),
        Err(UpdateError::InvalidSignature)
    ));
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
                UpdateServiceMode::Restarted => {
                    Ok(signed_update("stable", 1, "0.2.0", 2_000_000_001))
                }
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
    let temporary = tempfile::tempdir().unwrap();
    let sequence_path = temporary.path().join("updates.json");
    let check = |fetcher, path| {
        UpdateCheck::with_sequence_store(
            fetcher,
            test_verifier(),
            "0.1.0",
            UpdateSequenceStore::at(path),
        )
    };
    let policy = || UpdatePolicy::enabled("https://updates.musheen.test/latest.json", 0);

    for service_mode in [UpdateServiceMode::Absent, UpdateServiceMode::Disconnected] {
        *mode.lock().unwrap() = service_mode;
        let result =
            futures_lite::future::block_on(check(fetcher.clone(), sequence_path.clone()).check(
                policy(),
                2_000_000_000,
                CancellationToken::new(),
            ));
        assert!(matches!(result, Err(UpdateError::Transport(_))));
        file_manager.show("usable");
    }

    *mode.lock().unwrap() = UpdateServiceMode::Slow;
    let slow_fetcher = fetcher.clone();
    let slow_sequence_path = sequence_path.clone();
    let slow = std::thread::spawn(move || {
        futures_lite::future::block_on(check(slow_fetcher, slow_sequence_path).check(
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
    let offer = futures_lite::future::block_on(check(fetcher, sequence_path).check(
        policy(),
        2_000_000_000,
        CancellationToken::new(),
    ))
    .unwrap();
    assert!(matches!(offer, UpdateOffer::Information(_)));
    file_manager.show("restarted");
    assert_eq!(file_manager.calls(), 4);
}
