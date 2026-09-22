use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::{
    PortalClient, PortalError, PortalRequest, PortalSelection, PortalTransport, SandboxState,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::io::{BufRead as _, BufReader};
#[cfg(unix)]
use std::process::{Child, Command, Stdio};

#[cfg(unix)]
struct PrivateBus {
    child: Child,
    address: String,
}

#[cfg(unix)]
impl PrivateBus {
    fn start() -> Self {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("portal tests require dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(!address.trim().is_empty());
        Self {
            child,
            address: address.trim().to_owned(),
        }
    }
}

#[cfg(unix)]
impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

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

#[cfg(unix)]
#[derive(Clone)]
struct FakeRequest {
    closed: async_channel::Sender<()>,
}

#[cfg(unix)]
#[zbus::interface(name = "org.freedesktop.portal.Request")]
impl FakeRequest {
    async fn close(&self) {
        let _ = self.closed.try_send(());
    }
}

#[cfg(unix)]
#[derive(Clone)]
struct FakeChooser {
    accepted: async_channel::Sender<()>,
    closed: async_channel::Sender<()>,
}

#[cfg(unix)]
#[zbus::interface(name = "org.freedesktop.portal.FileChooser")]
impl FakeChooser {
    async fn open_file(
        &self,
        _parent: &str,
        _title: &str,
        mut options: std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedObjectPath> {
        let token = options
            .remove("handle_token")
            .and_then(|value| String::try_from(value).ok())
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("missing handle token".into()))?;
        let sender = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::Failed("missing sender".into()))?
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        let path: zbus::zvariant::OwnedObjectPath =
            format!("/org/freedesktop/portal/desktop/request/{sender}/{token}")
                .try_into()
                .map_err(|error: zbus::zvariant::Error| {
                    zbus::fdo::Error::InvalidArgs(error.to_string())
                })?;
        server
            .at(
                path.clone(),
                FakeRequest {
                    closed: self.closed.clone(),
                },
            )
            .await
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        let _ = self.accepted.try_send(());
        Ok(path)
    }
}

#[cfg(unix)]
#[test]
fn production_transport_closes_the_exact_portal_request_on_cancellation() {
    let bus = PrivateBus::start();
    futures_lite::future::block_on(async {
        let (accepted_tx, accepted_rx) = async_channel::bounded(1);
        let (closed_tx, closed_rx) = async_channel::bounded(1);
        let _service = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name("org.example.Portal")
            .unwrap()
            .serve_at(
                "/org/freedesktop/portal/desktop",
                FakeChooser {
                    accepted: accepted_tx,
                    closed: closed_tx,
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        let client_connection = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let client = PortalClient::new(
            musheen_desktop::AshpdPortalTransport::with_connection(
                client_connection,
                "org.example.Portal",
            ),
            SandboxState::Host,
            None,
        );
        let (result, ()) = futures_lite::future::zip(
            client.choose(PortalRequest::open("Open"), cancellation),
            async move {
                accepted_rx.recv().await.unwrap();
                cancel.cancel();
                closed_rx.recv().await.unwrap();
            },
        )
        .await;
        assert_eq!(result, Err(PortalError::Cancelled));
    });
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

    struct LateConfirmingUi {
        started: async_channel::Sender<()>,
    }

    impl BackendChooserUi for LateConfirmingUi {
        fn choose(
            &self,
            _request: BackendChooserRequest,
            cancellation: CancellationToken,
        ) -> BoxFuture<'static, Result<BackendChooserDecision, PortalError>> {
            let started = self.started.clone();
            Box::pin(async move {
                let _ = started.try_send(());
                poll_fn(move |context| {
                    if cancellation.is_cancelled() {
                        Poll::Ready(())
                    } else {
                        cancellation.register_waker(context.waker());
                        Poll::Pending
                    }
                })
                .await;
                Ok(BackendChooserDecision::Confirmed(vec![PathBuf::from(
                    "/tmp/too-late",
                )]))
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

    #[cfg(unix)]
    #[test]
    fn backend_service_is_exported_on_a_private_bus() {
        let bus = PrivateBus::start();
        futures_lite::future::block_on(async {
            let _service = musheen_desktop::serve_file_chooser_backend(
                Some(&bus.address),
                Arc::new(ConfirmingUi),
                "org.example.MusheenPortal",
                "com.github.musheen.Musheen",
            )
            .await
            .unwrap();
            let client = zbus::connection::Builder::address(bus.address.as_str())
                .unwrap()
                .build()
                .await
                .unwrap();
            let xml = zbus::fdo::IntrospectableProxy::builder(&client)
                .destination("org.example.MusheenPortal")
                .unwrap()
                .path("/org/freedesktop/portal/desktop")
                .unwrap()
                .build()
                .await
                .unwrap()
                .introspect()
                .await
                .unwrap();
            assert!(xml.contains("org.freedesktop.impl.portal.FileChooser"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn exported_backend_close_ignores_a_late_confirmation() {
        let bus = PrivateBus::start();
        futures_lite::future::block_on(async {
            let (started_tx, started_rx) = async_channel::bounded(1);
            let _service = musheen_desktop::serve_file_chooser_backend(
                Some(&bus.address),
                Arc::new(LateConfirmingUi {
                    started: started_tx,
                }),
                "org.example.MusheenPortal",
                "com.github.musheen.Musheen",
            )
            .await
            .unwrap();
            let client = zbus::connection::Builder::address(bus.address.as_str())
                .unwrap()
                .build()
                .await
                .unwrap();
            let chooser = zbus::Proxy::new(
                &client,
                "org.example.MusheenPortal",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.impl.portal.FileChooser",
            )
            .await
            .unwrap();
            let handle: zbus::zvariant::OwnedObjectPath =
                "/org/freedesktop/portal/desktop/request/test/cancelled"
                    .try_into()
                    .unwrap();
            let call_handle = handle.clone();
            let call = async move {
                chooser
                    .call_method(
                        "OpenFile",
                        &(
                            call_handle,
                            ashpd::zvariant::Optional::from(Some(MaybeAppID::from(
                                "org.example.Caller",
                            ))),
                            ashpd::zvariant::Optional::<ashpd::WindowIdentifierType>::default(),
                            "Open",
                            OpenFileOptions::default(),
                        ),
                    )
                    .await
                    .unwrap()
            };
            let close_client = client.clone();
            let close = async move {
                started_rx.recv().await.unwrap();
                let request = zbus::Proxy::new(
                    &close_client,
                    "org.example.MusheenPortal",
                    handle,
                    "org.freedesktop.impl.portal.Request",
                )
                .await
                .unwrap();
                request.call_method("Close", &()).await.unwrap();
            };
            let (reply, ()) = futures_lite::future::zip(call, close).await;
            let response = reply
                .body()
                .deserialize::<ashpd::desktop::Response<ashpd::desktop::file_chooser::SelectedFiles>>()
                .unwrap();
            assert!(matches!(response, ashpd::desktop::Response::Err(_)));
        });
    }
}
