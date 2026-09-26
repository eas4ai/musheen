//! An in-process SSH server with an SFTP subsystem, an SSH agent, and key
//! helpers for the SFTP login tests. Nothing here reads the user's ~/.ssh.

use russh::keys::ssh_key::{LineEnding, private::Ed25519Keypair};
use russh::keys::{PrivateKey, PublicKey, PublicKeyBase64};
use russh::server::{Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use russh_sftp::protocol::{Attrs, File, FileAttributes, Handle, Name, Status, StatusCode, Version};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// A deterministic Ed25519 key, so tests need no random source for keys.
pub(crate) fn ed25519_key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

/// An RSA key pair assembled from made-up numbers. It is not a working RSA
/// key; it has the shape of one, so a login can be shown to refuse it without
/// any RSA private-key code in the build.
pub(crate) fn fabricated_rsa_key() -> PrivateKey {
    use russh::keys::ssh_key::Mpint;
    use russh::keys::ssh_key::private::{KeypairData, RsaKeypair, RsaPrivateKey};
    use russh::keys::ssh_key::public::RsaPublicKey;
    let number = |first: u8| {
        let mut bytes = vec![0x5a; 256];
        bytes[0] = first;
        Mpint::from_positive_bytes(&bytes)
    };
    let public = RsaPublicKey::new(
        Mpint::from_positive_bytes(&[0x01, 0x00, 0x01]),
        number(0xc3),
    )
    .expect("the test modulus is positive");
    let private = RsaPrivateKey::new(number(0x41), number(0x42), number(0x43), number(0x44))
        .expect("the test numbers are positive");
    PrivateKey::new(
        KeypairData::Rsa(RsaKeypair::new(public, private).expect("the parts form a pair")),
        "fabricated test key",
    )
    .expect("the fabricated key encodes")
}

/// The SHA-256 host-key pin Musheen stores for `key`.
pub(crate) fn host_key_pin(key: &PrivateKey) -> [u8; 32] {
    let digest = Sha256::digest(key.public_key().public_key_bytes());
    let mut pin = [0; 32];
    pin.copy_from_slice(&digest);
    pin
}

/// Writes `key` in OpenSSH format, encrypted when `passphrase` is given.
pub(crate) fn write_key_file(path: &Path, key: &PrivateKey, passphrase: Option<&str>) {
    let key = match passphrase {
        Some(passphrase) => key
            .encrypt(&mut rand::rng(), passphrase)
            .expect("the test key encrypts"),
        None => key.clone(),
    };
    let text = key
        .to_openssh(LineEnding::LF)
        .expect("the test key encodes");
    std::fs::write(path, text.as_bytes()).expect("the test key file writes");
}

/// A known_hosts line that trusts `key` for `host` on `port`.
pub(crate) fn known_hosts_line(host: &str, port: u16, key: &PrivateKey) -> String {
    let public = key.public_key();
    format!(
        "[{host}]:{port} {} {}\n",
        public.algorithm().as_str(),
        public.public_key_base64()
    )
}

/// What a server accepts and what it saw.
#[derive(Clone, Default)]
pub(crate) struct ServerLog {
    inner: Arc<Mutex<LogState>>,
}

#[derive(Default)]
struct LogState {
    logins: Vec<String>,
    forwards: Vec<(String, u32)>,
}

impl ServerLog {
    /// The accepted logins, as "user password" or "user key <base64>".
    pub(crate) fn logins(&self) -> Vec<String> {
        self.state().logins.clone()
    }

    /// The host and port of each direct-tcpip channel a jump host opened.
    pub(crate) fn forwards(&self) -> Vec<(String, u32)> {
        self.state().forwards.clone()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, LogState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The logins a test server accepts.
#[derive(Clone, Default)]
pub(crate) struct Accepts {
    pub(crate) user: String,
    pub(crate) password: Option<String>,
    pub(crate) keys: Vec<PublicKey>,
    /// Lets clients open direct-tcpip channels, as a jump host does.
    pub(crate) forwarding: bool,
}

/// An SSH server on 127.0.0.1 with a root that lists one file, `hello.txt`.
pub(crate) struct TestSshServer {
    runtime: tokio::runtime::Runtime,
    port: u16,
    host_key: PrivateKey,
    log: ServerLog,
}

impl TestSshServer {
    pub(crate) fn start(host_key: PrivateKey, accepts: Accepts) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("the test server runtime starts");
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("the test server binds");
        let port = listener.local_addr().unwrap().port();
        let mut methods = MethodSet::empty();
        methods.push(MethodKind::Password);
        methods.push(MethodKind::PublicKey);
        let config = Arc::new(russh::server::Config {
            methods,
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            keys: vec![host_key.clone()],
            inactivity_timeout: Some(Duration::from_secs(30)),
            ..Default::default()
        });
        let log = ServerLog::default();
        let session_log = log.clone();
        runtime.spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let handler = TestSession {
                    accepts: accepts.clone(),
                    log: session_log.clone(),
                    channels: HashMap::new(),
                };
                let config = Arc::clone(&config);
                tokio::spawn(async move {
                    if let Ok(session) = russh::server::run_stream(config, socket, handler).await {
                        let _ = session.await;
                    }
                });
            }
        });
        Self {
            runtime,
            port,
            host_key,
            log,
        }
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn host_key(&self) -> &PrivateKey {
        &self.host_key
    }

    pub(crate) fn log(&self) -> &ServerLog {
        &self.log
    }
}

impl Drop for TestSshServer {
    fn drop(&mut self) {
        let runtime = std::mem::replace(
            &mut self.runtime,
            tokio::runtime::Builder::new_current_thread().build().unwrap(),
        );
        runtime.shutdown_background();
    }
}

struct TestSession {
    accepts: Accepts,
    log: ServerLog,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl russh::server::Handler for TestSession {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == self.accepts.user && self.accepts.password.as_deref() == Some(password) {
            self.log.state().logins.push(format!("{user} password"));
            return Ok(Auth::Accept);
        }
        Ok(Auth::reject())
    }

    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(self.accepts_key(user, public_key))
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        let auth = self.accepts_key(user, public_key);
        if matches!(auth, Auth::Accept) {
            self.log
                .state()
                .logins
                .push(format!("{user} key {}", public_key.public_key_base64()));
        }
        Ok(auth)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if !self.accepts.forwarding {
            return Ok(());
        }
        self.log
            .state()
            .forwards
            .push((host_to_connect.to_owned(), port_to_connect));
        let Ok(port) = u16::try_from(port_to_connect) else {
            return Ok(());
        };
        let Ok(mut target) = tokio::net::TcpStream::connect((host_to_connect, port)).await else {
            return Ok(());
        };
        reply.accept().await;
        tokio::spawn(async move {
            let mut stream = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut target).await;
        });
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        match (name, self.channels.remove(&channel_id)) {
            ("sftp", Some(channel)) => {
                session.channel_success(channel_id)?;
                tokio::spawn(russh_sftp::server::run(
                    channel.into_stream(),
                    TestSftp::default(),
                ));
            }
            _ => session.channel_failure(channel_id)?,
        }
        Ok(())
    }
}

impl TestSession {
    fn accepts_key(&self, user: &str, public_key: &PublicKey) -> Auth {
        let known = self
            .accepts
            .keys
            .iter()
            .any(|key| key.key_data() == public_key.key_data());
        if user == self.accepts.user && known {
            Auth::Accept
        } else {
            Auth::reject()
        }
    }
}

#[derive(Default)]
struct TestSftp {
    listed: bool,
}

fn directory_attributes() -> FileAttributes {
    FileAttributes::dummy()
}

fn file_attributes() -> FileAttributes {
    let mut attributes = FileAttributes::empty();
    attributes.size = Some(5);
    attributes.permissions = Some(0o644);
    attributes.set_regular(true);
    attributes
}

impl russh_sftp::server::Handler for TestSftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, _path: String) -> Result<Name, Self::Error> {
        Ok(Name {
            id,
            files: vec![File::dummy("/")],
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.attributes(id, &path)
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.attributes(id, &path)
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        self.listed = false;
        Ok(Handle { id, handle: path })
    }

    async fn readdir(&mut self, id: u32, _handle: String) -> Result<Name, Self::Error> {
        if self.listed {
            return Err(StatusCode::Eof);
        }
        self.listed = true;
        Ok(Name {
            id,
            files: vec![File::new("hello.txt", file_attributes())],
        })
    }

    async fn close(&mut self, id: u32, _handle: String) -> Result<Status, Self::Error> {
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_owned(),
            language_tag: "en-US".to_owned(),
        })
    }
}

impl TestSftp {
    fn attributes(&self, id: u32, path: &str) -> Result<Attrs, StatusCode> {
        let trimmed = path.trim_end_matches('/');
        let attrs = match trimmed {
            "" | "." => directory_attributes(),
            "/hello.txt" | "hello.txt" => file_attributes(),
            _ => return Err(StatusCode::NoSuchFile),
        };
        Ok(Attrs { id, attrs })
    }
}

/// An SSH agent on a Unix socket that holds `keys`, served by russh's agent
/// server.
pub(crate) struct TestAgent {
    runtime: tokio::runtime::Runtime,
    socket: std::path::PathBuf,
}

#[derive(Clone)]
struct AcceptAll;

impl russh::keys::agent::server::Agent for AcceptAll {}

impl TestAgent {
    pub(crate) fn start(directory: &Path, keys: &[PrivateKey]) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("the test agent runtime starts");
        let socket = directory.join("agent.sock");
        let listener = runtime
            .block_on(async { tokio::net::UnixListener::bind(&socket) })
            .expect("the test agent binds");
        let incoming = futures_lite::stream::unfold(listener, |listener| async move {
            let next = listener.accept().await.map(|(stream, _)| stream);
            Some((next, listener))
        });
        runtime.spawn(async move {
            let _ = russh::keys::agent::server::serve(Box::pin(incoming), AcceptAll).await;
        });
        let path = socket.clone();
        let keys = keys.to_vec();
        runtime.block_on(async move {
            let stream = tokio::net::UnixStream::connect(&path)
                .await
                .expect("the test agent accepts");
            let mut client = russh::keys::agent::client::AgentClient::connect(stream);
            for key in &keys {
                client
                    .add_identity(key, &[])
                    .await
                    .expect("the test agent takes the key");
            }
        });
        Self { runtime, socket }
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for TestAgent {
    fn drop(&mut self) {
        let runtime = std::mem::replace(
            &mut self.runtime,
            tokio::runtime::Builder::new_current_thread().build().unwrap(),
        );
        runtime.shutdown_background();
    }
}

/// An agent that offers one RSA public key and records the flags of every
/// signature request, refusing to sign. It shows which hash Musheen asks for
/// without any RSA private-key code in the build.
pub(crate) struct RecordingRsaAgent {
    socket: std::path::PathBuf,
    flags: Arc<Mutex<Vec<u32>>>,
}

impl RecordingRsaAgent {
    pub(crate) fn start(directory: &Path) -> Self {
        use std::io::{Read, Write};
        let socket = directory.join("rsa-agent.sock");
        let listener =
            std::os::unix::net::UnixListener::bind(&socket).expect("the RSA agent binds");
        let flags = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&flags);
        let blob = rsa_public_key_blob();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                loop {
                    let mut length = [0; 4];
                    if stream.read_exact(&mut length).is_err() {
                        break;
                    }
                    let mut message = vec![0; u32::from_be_bytes(length) as usize];
                    if stream.read_exact(&mut message).is_err() {
                        break;
                    }
                    let reply = match message.first() {
                        // SSH_AGENTC_REQUEST_IDENTITIES
                        Some(11) => {
                            let mut body = vec![12];
                            body.extend_from_slice(&1u32.to_be_bytes());
                            body.extend_from_slice(&(blob.len() as u32).to_be_bytes());
                            body.extend_from_slice(&blob);
                            body.extend_from_slice(&0u32.to_be_bytes());
                            body
                        }
                        // SSH_AGENTC_SIGN_REQUEST: key, data, flags
                        Some(13) => {
                            if let Some(flag) = sign_request_flags(&message[1..]) {
                                recorded
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .push(flag);
                            }
                            vec![5]
                        }
                        _ => vec![5],
                    };
                    let mut framed = (reply.len() as u32).to_be_bytes().to_vec();
                    framed.extend_from_slice(&reply);
                    if stream.write_all(&framed).is_err() {
                        break;
                    }
                }
            }
        });
        Self { socket, flags }
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    /// The flags of each signature request; 2 asks for rsa-sha2-256, 4 for
    /// rsa-sha2-512, and 0 for SHA-1.
    pub(crate) fn sign_flags(&self) -> Vec<u32> {
        self.flags
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn sign_request_flags(body: &[u8]) -> Option<u32> {
    let skip = |bytes: &[u8]| -> Option<usize> {
        let length = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
        Some(4 + length)
    };
    let key = skip(body)?;
    let data = skip(body.get(key..)?)?;
    let flags = body.get(key + data..key + data + 4)?;
    Some(u32::from_be_bytes(flags.try_into().ok()?))
}

/// An ssh-rsa public key blob with a 2048-bit modulus. Only its shape
/// matters: nothing ever verifies a signature against it.
fn rsa_public_key_blob() -> Vec<u8> {
    let mut modulus = vec![0; 257];
    modulus[1] = 0xc3;
    modulus[256] = 0x01;
    let mut blob = Vec::new();
    for field in [b"ssh-rsa".as_slice(), &[0x01, 0x00, 0x01], &modulus] {
        blob.extend_from_slice(&(field.len() as u32).to_be_bytes());
        blob.extend_from_slice(field);
    }
    blob
}
