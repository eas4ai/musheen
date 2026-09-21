use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::{
    PortalClient, PortalError, PortalRequest, PortalSelection, PortalTransport, SandboxState,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct FakePortal {
    result: Arc<Mutex<Option<Result<PortalSelection, PortalError>>>>,
}

impl PortalTransport for FakePortal {
    fn choose(
        &self,
        _request: PortalRequest,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<PortalSelection, PortalError>> {
        let result = self.result.lock().unwrap().take().unwrap();
        Box::pin(async move { result })
    }
}

#[test]
fn sandbox_selection_requires_document_grants() {
    let transport = FakePortal {
        result: Arc::new(Mutex::new(Some(Ok(PortalSelection::new(
            vec![PathBuf::from("/run/user/1000/doc/abc/file.txt")],
            false,
        ))))),
    };
    let client = PortalClient::new(transport, SandboxState::Sandboxed, None);
    let result = futures_lite::future::block_on(
        client.choose(PortalRequest::open("Open"), CancellationToken::new()),
    );
    assert!(matches!(result, Err(PortalError::MissingDocumentGrant)));
}

#[test]
fn cancellation_is_preserved_as_a_typed_result() {
    let transport = FakePortal {
        result: Arc::new(Mutex::new(Some(Err(PortalError::Cancelled)))),
    };
    let client = PortalClient::new(transport, SandboxState::Host, None);
    let result = futures_lite::future::block_on(
        client.choose(PortalRequest::save("Save"), CancellationToken::new()),
    );
    assert!(matches!(result, Err(PortalError::Cancelled)));
}

#[test]
fn client_refuses_to_route_into_its_own_optional_backend() {
    let transport = FakePortal {
        result: Arc::new(Mutex::new(Some(Ok(PortalSelection::new(
            vec![PathBuf::from("/tmp/file")],
            true,
        ))))),
    };
    let client = PortalClient::new(
        transport,
        SandboxState::Host,
        Some("org.freedesktop.impl.portal.desktop.musheen".into()),
    );
    let result = futures_lite::future::block_on(
        client.choose(
            PortalRequest::open("Open")
                .with_backend_destination("org.freedesktop.impl.portal.desktop.musheen"),
            CancellationToken::new(),
        ),
    );
    assert!(matches!(result, Err(PortalError::RecursiveBackend)));
}

#[cfg(feature = "portal-backend")]
mod backend {
    use super::*;
    use ashpd::MaybeAppID;
    use ashpd::backend::file_chooser::FileChooserImpl as _;
    use ashpd::backend::request::RequestImpl as _;
    use ashpd::desktop::HandleToken;
    use ashpd::desktop::file_chooser::OpenFileOptions;
    use musheen_desktop::{
        AshpdFileChooserBackend, BackendChooserDecision, BackendChooserRequest, BackendChooserUi,
    };
    use std::future::poll_fn;
    use std::task::Poll;

    struct ConfirmingUi;

    impl BackendChooserUi for ConfirmingUi {
        fn choose(
            &self,
            _request: BackendChooserRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'static, Result<BackendChooserDecision, PortalError>> {
            Box::pin(async {
                Ok(BackendChooserDecision::Confirmed(vec![PathBuf::from(
                    "/tmp/a b.txt",
                )]))
            })
        }
    }

    struct CancellingUi;

    impl BackendChooserUi for CancellingUi {
        fn choose(
            &self,
            _request: BackendChooserRequest,
            cancellation: CancellationToken,
        ) -> BoxFuture<'static, Result<BackendChooserDecision, PortalError>> {
            Box::pin(async move {
                poll_fn(move |context| {
                    if cancellation.is_cancelled() {
                        Poll::Ready(())
                    } else {
                        cancellation.register_waker(context.waker());
                        Poll::Pending
                    }
                })
                .await;
                Ok(BackendChooserDecision::Cancelled)
            })
        }
    }

    #[test]
    fn backend_returns_only_confirmed_selection_and_refuses_self_call() {
        let backend =
            AshpdFileChooserBackend::new(Arc::new(ConfirmingUi), "com.github.musheen.Musheen");
        let token: HandleToken = "confirmed".parse().unwrap();
        let selected = futures_lite::future::block_on(backend.open_file(
            token,
            Some(MaybeAppID::from("org.example.Caller")),
            None,
            "Open",
            OpenFileOptions::default(),
        ))
        .unwrap();
        assert_eq!(selected.uris()[0].as_str(), "file:///tmp/a%20b.txt");

        let token: HandleToken = "self_call".parse().unwrap();
        let refused = futures_lite::future::block_on(backend.open_file(
            token,
            Some(MaybeAppID::from("com.github.musheen.Musheen")),
            None,
            "Open",
            OpenFileOptions::default(),
        ));
        assert!(matches!(refused, Err(ashpd::PortalError::NotAllowed(_))));
    }

    #[test]
    fn backend_close_cancels_the_exact_request() {
        let backend = Arc::new(AshpdFileChooserBackend::new(
            Arc::new(CancellingUi),
            "com.github.musheen.Musheen",
        ));
        let token: HandleToken = "cancel_me".parse().unwrap();
        let open_backend = Arc::clone(&backend);
        let open_token = token.clone();
        let close_backend = Arc::clone(&backend);
        let (result, ()) = futures_lite::future::block_on(futures_lite::future::zip(
            async move {
                open_backend
                    .open_file(
                        open_token,
                        Some(MaybeAppID::from("org.example.Caller")),
                        None,
                        "Open",
                        OpenFileOptions::default(),
                    )
                    .await
            },
            async move {
                futures_lite::future::yield_now().await;
                close_backend.close(token).await;
            },
        ));
        assert!(matches!(result, Err(ashpd::PortalError::Cancelled(_))));
    }
}
