mod support;

use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::{
    PortalClient, PortalError, PortalRequest, PortalSelection, PortalTransport, SandboxState,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use support::UsableFileManager;

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

#[derive(Clone, Copy)]
enum PortalServiceMode {
    Absent,
    Slow,
    Disconnected,
    Restarted,
}

#[derive(Clone)]
struct MatrixPortal {
    mode: Arc<Mutex<PortalServiceMode>>,
    slow_started: async_channel::Sender<()>,
    slow_release: async_channel::Receiver<()>,
}

impl PortalTransport for MatrixPortal {
    fn choose(
        &self,
        _request: PortalRequest,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<PortalSelection, PortalError>> {
        let mode = *self.mode.lock().unwrap();
        let slow_started = self.slow_started.clone();
        let slow_release = self.slow_release.clone();
        Box::pin(async move {
            match mode {
                PortalServiceMode::Absent => Err(PortalError::Unavailable("absent".into())),
                PortalServiceMode::Disconnected => {
                    Err(PortalError::Unavailable("disconnected".into()))
                }
                PortalServiceMode::Slow => {
                    slow_started.send(()).await.unwrap();
                    slow_release.recv().await.unwrap();
                    Err(PortalError::Unavailable("slow".into()))
                }
                PortalServiceMode::Restarted => Ok(PortalSelection::new(
                    vec![PathBuf::from("/tmp/restarted-selection")],
                    true,
                )),
            }
        })
    }
}

#[test]
fn portal_absence_slowness_disconnect_and_restart_do_not_block_file_management() {
    let file_manager = UsableFileManager::new();
    let (slow_started, slow_ready) = async_channel::bounded(1);
    let (slow_release, release) = async_channel::bounded(1);
    let mode = Arc::new(Mutex::new(PortalServiceMode::Absent));
    let client = Arc::new(PortalClient::new(
        MatrixPortal {
            mode: Arc::clone(&mode),
            slow_started,
            slow_release: release,
        },
        SandboxState::Host,
        None,
    ));

    for service_mode in [PortalServiceMode::Absent, PortalServiceMode::Disconnected] {
        *mode.lock().unwrap() = service_mode;
        let result = futures_lite::future::block_on(
            client.choose(PortalRequest::open("Open"), CancellationToken::new()),
        );
        assert!(matches!(result, Err(PortalError::Unavailable(_))));
        file_manager.show("usable");
    }

    *mode.lock().unwrap() = PortalServiceMode::Slow;
    let slow_client = Arc::clone(&client);
    let slow = std::thread::spawn(move || {
        futures_lite::future::block_on(
            slow_client.choose(PortalRequest::open("Open"), CancellationToken::new()),
        )
    });
    slow_ready.recv_blocking().unwrap();
    file_manager.show("still-usable");
    slow_release.send_blocking(()).unwrap();
    assert!(matches!(
        slow.join().unwrap(),
        Err(PortalError::Unavailable(_))
    ));

    *mode.lock().unwrap() = PortalServiceMode::Restarted;
    let selection = futures_lite::future::block_on(
        client.choose(PortalRequest::open("Open"), CancellationToken::new()),
    )
    .unwrap();
    assert_eq!(
        selection.paths(),
        [PathBuf::from("/tmp/restarted-selection")]
    );
    file_manager.show("restarted");
    assert_eq!(file_manager.calls(), 4);
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
fn portal_backend_client_never_routes_into_its_own_backend() {
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
    use std::ffi::OsStr;
    use std::future::poll_fn;
    use std::task::Poll;

    /// Confirms `path`.
    struct ConfirmingUi {
        path: PathBuf,
    }

    impl BackendChooserUi for ConfirmingUi {
        fn choose(
            &self,
            _request: BackendChooserRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'static, Result<BackendChooserDecision, PortalError>> {
            let path = self.path.clone();
            Box::pin(async move { Ok(BackendChooserDecision::Confirmed(vec![path])) })
        }
    }

    /// A folder holding the file `a b.txt`, the file, and its URI: the
    /// backend returns only paths that exist and fit the request.
    fn confirmed_file() -> (tempfile::TempDir, PathBuf, String) {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("a b.txt");
        std::fs::write(&file, b"a").unwrap();
        let uri = format!("file://{}/a%20b.txt", folder.path().display());
        (folder, file, uri)
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
    fn portal_backend_returns_only_confirmed_selection_and_refuses_self_call() {
        let (_folder, file, uri) = confirmed_file();
        let backend = AshpdFileChooserBackend::new(
            Arc::new(ConfirmingUi { path: file }),
            "com.github.musheen.Musheen",
        );
        let token: HandleToken = "confirmed".parse().unwrap();
        let selected = futures_lite::future::block_on(backend.open_file(
            token,
            Some(MaybeAppID::from("org.example.Caller")),
            None,
            "Open",
            OpenFileOptions::default(),
        ))
        .unwrap();
        assert_eq!(selected.uris()[0].as_str(), uri);

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
    fn portal_backend_close_cancels_the_exact_request() {
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
                Arc::new(ConfirmingUi {
                    path: PathBuf::from("/"),
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
    fn portal_backend_close_from_the_portal_cancels_and_ignores_a_late_confirmation() {
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
            // The portal service calls, and may close its own request.
            let client = bus_caller(&bus, true).await;
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

    /// Confirms `path` once `confirm` receives, unless the request is
    /// cancelled first.
    struct SignalledUi {
        started: async_channel::Sender<()>,
        confirm: async_channel::Receiver<()>,
        path: PathBuf,
    }

    impl BackendChooserUi for SignalledUi {
        fn choose(
            &self,
            _request: BackendChooserRequest,
            cancellation: CancellationToken,
        ) -> BoxFuture<'static, Result<BackendChooserDecision, PortalError>> {
            let (started, confirm) = (self.started.clone(), self.confirm.clone());
            let path = self.path.clone();
            Box::pin(async move {
                let _ = started.try_send(());
                futures_lite::future::race(
                    async move {
                        let _ = confirm.recv().await;
                        Ok(BackendChooserDecision::Confirmed(vec![path]))
                    },
                    async move {
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
                    },
                )
                .await
            })
        }
    }

    const PORTAL_SERVICE: &str = "org.freedesktop.portal.Desktop";
    const BACKEND: &str = "org.example.MusheenPortal";
    const NOT_ALLOWED: &str = "org.freedesktop.portal.Error.NotAllowed";

    /// A connection to `bus`; with `portal`, it owns the portal service's
    /// name, as xdg-desktop-portal does.
    #[cfg(unix)]
    async fn bus_caller(bus: &PrivateBus, portal: bool) -> zbus::Connection {
        let builder = zbus::connection::Builder::address(bus.address.as_str()).unwrap();
        let builder = if portal {
            builder.name(PORTAL_SERVICE).unwrap()
        } else {
            builder
        };
        builder.build().await.unwrap()
    }

    #[cfg(unix)]
    fn request_handle(name: &str) -> zbus::zvariant::OwnedObjectPath {
        format!("/org/freedesktop/portal/desktop/request/test/{name}")
            .try_into()
            .unwrap()
    }

    #[cfg(unix)]
    async fn call_open_file(
        connection: &zbus::Connection,
        name: &str,
    ) -> zbus::Result<zbus::Message> {
        zbus::Proxy::new(
            connection,
            BACKEND,
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.impl.portal.FileChooser",
        )
        .await
        .unwrap()
        .call_method(
            "OpenFile",
            &(
                request_handle(name),
                ashpd::zvariant::Optional::from(Some(MaybeAppID::from("org.example.Caller"))),
                ashpd::zvariant::Optional::<ashpd::WindowIdentifierType>::default(),
                "Open",
                OpenFileOptions::default(),
            ),
        )
        .await
    }

    #[cfg(unix)]
    fn refused(result: &zbus::Result<zbus::Message>) -> bool {
        matches!(result, Err(zbus::Error::MethodError(name, _, _)) if name.as_str() == NOT_ALLOWED)
    }

    #[cfg(unix)]
    #[test]
    fn portal_backend_answers_only_the_portal_service() {
        let bus = PrivateBus::start();
        futures_lite::future::block_on(async {
            let (_folder, file, uri) = confirmed_file();
            let _service = musheen_desktop::serve_file_chooser_backend(
                Some(&bus.address),
                Arc::new(ConfirmingUi { path: file }),
                BACKEND,
                "com.github.musheen.Musheen",
            )
            .await
            .unwrap();
            let stranger = bus_caller(&bus, false).await;
            let answer = call_open_file(&stranger, "stranger").await;
            assert!(refused(&answer), "a stranger is refused: {answer:?}");

            let portal = bus_caller(&bus, true).await;
            let reply = call_open_file(&portal, "portal").await.unwrap();
            let response = reply
                .body()
                .deserialize::<ashpd::desktop::Response<ashpd::desktop::file_chooser::SelectedFiles>>()
                .unwrap();
            let ashpd::desktop::Response::Ok(selected) = response else {
                panic!("the portal service is answered");
            };
            assert_eq!(selected.uris()[0].as_str(), uri);
        });
    }

    #[cfg(unix)]
    #[test]
    fn portal_backend_refuses_close_from_other_callers() {
        let bus = PrivateBus::start();
        futures_lite::future::block_on(async {
            let (started_tx, started_rx) = async_channel::bounded(1);
            let (confirm_tx, confirm_rx) = async_channel::bounded(1);
            let (_folder, file, _) = confirmed_file();
            let _service = musheen_desktop::serve_file_chooser_backend(
                Some(&bus.address),
                Arc::new(SignalledUi {
                    started: started_tx,
                    confirm: confirm_rx,
                    path: file,
                }),
                BACKEND,
                "com.github.musheen.Musheen",
            )
            .await
            .unwrap();
            let portal = bus_caller(&bus, true).await;
            let stranger = bus_caller(&bus, false).await;
            let call = call_open_file(&portal, "pending");
            let close = async {
                started_rx.recv().await.unwrap();
                let closed = zbus::Proxy::new(
                    &stranger,
                    BACKEND,
                    request_handle("pending"),
                    "org.freedesktop.impl.portal.Request",
                )
                .await
                .unwrap()
                .call_method("Close", &())
                .await;
                confirm_tx.send(()).await.unwrap();
                closed
            };
            let (reply, closed) = futures_lite::future::zip(call, close).await;
            assert!(refused(&closed), "a stranger may not close: {closed:?}");
            let response = reply
                .unwrap()
                .body()
                .deserialize::<ashpd::desktop::Response<ashpd::desktop::file_chooser::SelectedFiles>>()
                .unwrap();
            assert!(
                matches!(response, ashpd::desktop::Response::Ok(_)),
                "the request was not cancelled"
            );
        });
    }

    /// Records each request and confirms `answer`.
    struct RecordingUi {
        requests: Arc<Mutex<Vec<BackendChooserRequest>>>,
        answer: Vec<PathBuf>,
    }

    impl BackendChooserUi for RecordingUi {
        fn choose(
            &self,
            request: BackendChooserRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'static, Result<BackendChooserDecision, PortalError>> {
            self.requests.lock().unwrap().push(request);
            let answer = self.answer.clone();
            Box::pin(async move { Ok(BackendChooserDecision::Confirmed(answer)) })
        }
    }

    /// `path` as the portal sends it.
    fn portal_file_path(path: &str) -> ashpd::FilePath {
        use serde::Deserialize as _;

        let mut bytes = path.as_bytes().to_vec();
        bytes.push(0);
        ashpd::FilePath::deserialize(serde::de::value::SeqDeserializer::<
            _,
            serde::de::value::Error,
        >::new(bytes.into_iter()))
        .unwrap()
    }

    #[test]
    fn portal_backend_requests_carry_the_callers_options() {
        use ashpd::desktop::file_chooser::{FileFilter, SaveFileOptions, SaveFilesOptions};

        let folder = tempfile::tempdir().unwrap();
        let (a, b) = (folder.path().join("a"), folder.path().join("b"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let two = AshpdFileChooserBackend::new(
            Arc::new(RecordingUi {
                requests: Arc::clone(&requests),
                answer: vec![a.clone(), b],
            }),
            "com.github.musheen.Musheen",
        );
        let caller = || Some(MaybeAppID::from("org.example.Caller"));
        let text = FileFilter::new("Text").glob("*.txt");
        let images = FileFilter::new("Images").mimetype("image/*");
        let options = OpenFileOptions::default()
            .set_multiple(true)
            .set_current_folder(portal_file_path("/tmp/start"))
            .set_filters([text])
            .set_current_filter(images);
        let selected = futures_lite::future::block_on(two.open_file(
            "open".parse().unwrap(),
            caller(),
            None,
            "Open",
            options,
        ))
        .unwrap();
        assert_eq!(selected.uris().len(), 2);
        let request = requests.lock().unwrap().pop().unwrap();
        assert!(request.multiple());
        assert!(!request.directory());
        assert_eq!(
            request.current_folder(),
            Some(std::path::Path::new("/tmp/start"))
        );
        assert_eq!(
            request
                .filters()
                .iter()
                .map(|filter| filter.label())
                .collect::<Vec<_>>(),
            ["Text", "Images"],
            "a current filter the list lacks is added"
        );
        assert_eq!(request.current_filter(), Some(1));

        let refused = futures_lite::future::block_on(two.open_file(
            "single".parse().unwrap(),
            caller(),
            None,
            "Open",
            OpenFileOptions::default(),
        ));
        assert!(
            matches!(refused, Err(ashpd::PortalError::InvalidArgument(_))),
            "two files for a request that asked for one are refused"
        );

        let save = futures_lite::future::block_on(two.save_file(
            "save".parse().unwrap(),
            caller(),
            None,
            "Save",
            SaveFileOptions::default().set_current_file(portal_file_path("/tmp/folder/report.txt")),
        ));
        assert!(save.is_err(), "Save returns one path");
        let request = requests.lock().unwrap().pop().unwrap();
        assert_eq!(
            request.current_folder(),
            Some(std::path::Path::new("/tmp/folder"))
        );
        assert_eq!(request.current_name(), Some(OsStr::new("report.txt")));

        let escaping = futures_lite::future::block_on(
            two.save_files(
                "save_many".parse().unwrap(),
                caller(),
                None,
                "Save",
                SaveFilesOptions::default()
                    .set_files([portal_file_path("a.txt"), portal_file_path("../b.txt")]),
            ),
        );
        assert!(
            matches!(escaping, Err(ashpd::PortalError::InvalidArgument(_))),
            "a name that could leave the folder is refused"
        );
    }

    #[test]
    fn portal_backend_filters_match_names_and_mime_types() {
        use ashpd::desktop::file_chooser::FileFilter;
        use musheen_desktop::ChooserFilter;
        use std::ffi::OsStr;

        let images = ChooserFilter::from(
            &FileFilter::new("Images")
                .glob("*.[pP][nN][gG]")
                .glob("photo-??.jpg")
                .mimetype("image/*"),
        );
        assert!(images.matches(OsStr::new("a.png"), None, None));
        assert!(images.matches(OsStr::new("A.PNG"), None, None));
        assert!(images.matches(OsStr::new("photo-01.jpg"), None, None));
        assert!(!images.matches(OsStr::new("photo-1.jpg"), None, None));
        assert!(images.matches(OsStr::new("scan"), Some("image/tiff"), None));
        assert!(!images.matches(OsStr::new("notes.txt"), Some("text/plain"), None));

        // With the MIME database, a subclass matches: C source is text.
        let text = ChooserFilter::from(&FileFilter::new("Text").mimetype("text/plain"));
        let types = musheen_desktop::NameMimeTypes::new();
        assert!(text.matches(OsStr::new("main.c"), Some("text/x-csrc"), Some(&types)));
        assert!(!text.matches(OsStr::new("a.png"), Some("image/png"), Some(&types)));
        assert!(images.needs_mime_types());

        let not_a = ChooserFilter::from(&FileFilter::new("Not a").glob("[!a]*"));
        assert!(not_a.matches(OsStr::new("b"), None, None));
        assert!(!not_a.matches(OsStr::new("a"), None, None));
        assert!(!not_a.needs_mime_types());
        let exact = ChooserFilter::from(&FileFilter::new("Make").glob("Makefile"));
        assert!(exact.matches(OsStr::new("Makefile"), None, None));
        assert!(!exact.matches(OsStr::new("Makefile.am"), None, None));
    }

    /// Returns `selection` and records each request.
    struct SelectingUi {
        requests: Arc<Mutex<Vec<BackendChooserRequest>>>,
        selection: musheen_desktop::ChooserSelection,
    }

    impl BackendChooserUi for SelectingUi {
        fn choose(
            &self,
            request: BackendChooserRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'static, Result<BackendChooserDecision, PortalError>> {
            self.requests.lock().unwrap().push(request);
            let selection = self.selection.clone();
            Box::pin(async move { Ok(BackendChooserDecision::Selected(selection)) })
        }
    }

    #[test]
    fn portal_backend_refuses_paths_of_the_wrong_kind() {
        let (folder, file, _) = confirmed_file();
        let answer = |path: PathBuf| {
            AshpdFileChooserBackend::new(
                Arc::new(ConfirmingUi { path }),
                "com.github.musheen.Musheen",
            )
        };
        let open = |backend: &AshpdFileChooserBackend<ConfirmingUi>, options| {
            futures_lite::future::block_on(backend.open_file(
                "kind".parse().unwrap(),
                Some(MaybeAppID::from("org.example.Caller")),
                None,
                "Open",
                options,
            ))
        };
        let folders = || OpenFileOptions::default().set_directory(true);
        let invalid = |result: &ashpd::backend::Result<_>| {
            matches!(result, Err(ashpd::PortalError::InvalidArgument(_)))
        };
        assert!(
            invalid(&open(&answer(file.clone()), folders())),
            "a file for a folder request"
        );
        assert!(
            invalid(&open(&answer(folder.path().join("missing")), folders())),
            "a missing path for a folder request"
        );
        assert!(open(&answer(folder.path().to_path_buf()), folders()).is_ok());
        assert!(
            invalid(&open(
                &answer(folder.path().to_path_buf()),
                OpenFileOptions::default()
            )),
            "a folder for a file request"
        );
        assert!(open(&answer(file), OpenFileOptions::default()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn portal_backend_close_cancels_only_the_request_it_names() {
        let bus = PrivateBus::start();
        futures_lite::future::block_on(async {
            let (started_tx, started_rx) = async_channel::bounded(2);
            let (confirm_tx, confirm_rx) = async_channel::bounded(1);
            let (_folder, file, uri) = confirmed_file();
            let _service = musheen_desktop::serve_file_chooser_backend(
                Some(&bus.address),
                Arc::new(SignalledUi {
                    started: started_tx,
                    confirm: confirm_rx,
                    path: file,
                }),
                BACKEND,
                "com.github.musheen.Musheen",
            )
            .await
            .unwrap();
            let portal = bus_caller(&bus, true).await;
            // Two apps chose the same token; the handles differ.
            let first = call_open_file(&portal, "app_a/token");
            let second = call_open_file(&portal, "app_b/token");
            let close = async {
                started_rx.recv().await.unwrap();
                started_rx.recv().await.unwrap();
                zbus::Proxy::new(
                    &portal,
                    BACKEND,
                    request_handle("app_a/token"),
                    "org.freedesktop.impl.portal.Request",
                )
                .await
                .unwrap()
                .call_method("Close", &())
                .await
                .unwrap();
                confirm_tx.send(()).await.unwrap();
            };
            let ((first, second), ()) =
                futures_lite::future::zip(futures_lite::future::zip(first, second), close).await;
            let response = |reply: zbus::Result<zbus::Message>| {
                reply
                    .unwrap()
                    .body()
                    .deserialize::<ashpd::desktop::Response<ashpd::desktop::file_chooser::SelectedFiles>>()
                    .unwrap()
            };
            assert!(
                matches!(response(first), ashpd::desktop::Response::Err(_)),
                "the closed request is cancelled"
            );
            let ashpd::desktop::Response::Ok(selected) = response(second) else {
                panic!("the other app's request is not cancelled");
            };
            assert_eq!(selected.uris()[0].as_str(), uri);
        });
    }

    #[cfg(unix)]
    #[test]
    fn portal_backend_replies_carry_writable_filter_and_choices() {
        use ashpd::desktop::file_chooser::{Choice, FileFilter};
        use std::collections::HashMap;
        use zbus::zvariant::OwnedValue;

        let bus = PrivateBus::start();
        futures_lite::future::block_on(async {
            let (_folder, file, uri) = confirmed_file();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let _service = musheen_desktop::serve_file_chooser_backend(
                Some(&bus.address),
                Arc::new(SelectingUi {
                    requests: Arc::clone(&requests),
                    selection: musheen_desktop::ChooserSelection {
                        paths: vec![file],
                        filter: Some(1),
                        choices: vec![("encoding".into(), "latin1".into())],
                    },
                }),
                BACKEND,
                "com.github.musheen.Musheen",
            )
            .await
            .unwrap();
            let portal = bus_caller(&bus, true).await;
            let options = OpenFileOptions::default()
                .set_filters([
                    FileFilter::new("Text").glob("*.txt"),
                    FileFilter::new("Images").mimetype("image/png"),
                ])
                .set_choices([Choice::new("encoding", "Encoding", "utf8")
                    .insert("utf8", "UTF-8")
                    .insert("latin1", "Latin-1")])
                .set_accept_label("Import");
            let reply = zbus::Proxy::new(
                &portal,
                BACKEND,
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.impl.portal.FileChooser",
            )
            .await
            .unwrap()
            .call_method(
                "OpenFile",
                &(
                    request_handle("fields"),
                    ashpd::zvariant::Optional::from(Some(MaybeAppID::from("org.example.Caller"))),
                    ashpd::zvariant::Optional::<ashpd::WindowIdentifierType>::default(),
                    "Open",
                    options,
                ),
            )
            .await
            .unwrap();
            let (code, results) = reply
                .body()
                .deserialize::<(u32, HashMap<String, OwnedValue>)>()
                .unwrap();
            assert_eq!(code, 0);
            assert!(format!("{:?}", results["uris"]).contains(&uri));
            assert!(results["writable"].downcast_ref::<bool>().unwrap());
            assert!(format!("{:?}", results["current_filter"]).contains("Images"));
            let choices = format!("{:?}", results["choices"]);
            assert!(choices.contains("encoding") && choices.contains("latin1"));

            let request = requests.lock().unwrap().pop().unwrap();
            assert_eq!(request.accept_label(), Some("Import"));
            let choice = &request.choices()[0];
            assert_eq!((choice.id(), choice.initial()), ("encoding", "utf8"));
            assert_eq!(choice.options().len(), 2);
        });
    }
}
