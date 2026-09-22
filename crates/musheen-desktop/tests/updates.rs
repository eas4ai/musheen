use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::{
    MetadataFetcher, UpdateCheck, UpdateError, UpdateMetadataVerifier, UpdateOffer, UpdatePolicy,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
