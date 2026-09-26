//! Test stand-ins for a remote server and the desktop secret service, so remote
//! tests never reach the network or the user's keyring.

use musheen_core::BoxFuture;
use musheen_desktop::{
    CredentialReference, MutationDispatch, SecretBuffer, SecretError, SecretServiceBackend,
    SecretServiceState,
};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// What a fake FTP server accepts.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FtpServerOptions {
    /// The password USER must follow with; `None` logs any user in at once.
    pub(crate) password: Option<&'static str>,
    /// Whether the root can be listed; when false every data connection is
    /// refused, as on a server whose passive ports a firewall blocks.
    pub(crate) lists_root: bool,
}

#[derive(Default)]
struct FtpLog {
    first_commands: Vec<String>,
    passwords: Vec<String>,
}

/// A plaintext FTP server on 127.0.0.1 whose root holds `hello.txt`. It
/// refuses `AUTH TLS` after recording it, so a TLS client stops there.
pub(crate) struct FakeFtp {
    port: u16,
    log: Arc<Mutex<FtpLog>>,
}

impl FakeFtp {
    pub(crate) fn start(options: FtpServerOptions) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the fake FTP server binds");
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(FtpLog::default()));
        let server_log = Arc::clone(&log);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let log = Arc::clone(&server_log);
                std::thread::spawn(move || serve_ftp(stream, options, &log));
            }
        });
        Self { port, log }
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// The first thing each client sent, as text.
    pub(crate) fn first_commands(&self) -> Vec<String> {
        self.state().first_commands.clone()
    }

    /// Every PASS argument the server received.
    pub(crate) fn passwords(&self) -> Vec<String> {
        self.state().passwords.clone()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FtpLog> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn serve_ftp(stream: TcpStream, options: FtpServerOptions, log: &Mutex<FtpLog>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let reply = |writer: &mut TcpStream, line: &str| writer.write_all(line.as_bytes()).is_ok();
    if !reply(&mut writer, "220 fake FTP ready\r\n") {
        return;
    }
    let mut data: Option<TcpListener> = None;
    let mut first = true;
    loop {
        let mut line = Vec::new();
        match (&mut reader).take(1024).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line).trim_end().to_owned();
        let (verb, argument) = text.split_once(' ').unwrap_or((text.as_str(), ""));
        let verb = verb.to_ascii_uppercase();
        {
            let mut state = log.lock().unwrap_or_else(PoisonError::into_inner);
            if first {
                state.first_commands.push(text.clone());
            }
            if verb == "PASS" {
                state.passwords.push(argument.to_owned());
            }
        }
        first = false;
        let answer = match verb.as_str() {
            "AUTH" => "534 TLS is not offered here\r\n".to_owned(),
            "USER" if options.password.is_some() => "331 password please\r\n".to_owned(),
            "USER" => "230 welcome\r\n".to_owned(),
            "PASS" if options.password == Some(argument) => "230 welcome\r\n".to_owned(),
            "PASS" => "530 wrong password\r\n".to_owned(),
            "CWD" => "250 ok\r\n".to_owned(),
            "PWD" => "257 \"/\"\r\n".to_owned(),
            "TYPE" | "OPTS" | "NOOP" => "200 ok\r\n".to_owned(),
            "SYST" => "215 UNIX Type: L8\r\n".to_owned(),
            "FEAT" => "211 End\r\n".to_owned(),
            "PASV" | "EPSV" if options.lists_root => {
                let listener = TcpListener::bind("127.0.0.1:0").expect("a data port binds");
                let port = listener.local_addr().unwrap().port();
                data = Some(listener);
                if verb == "PASV" {
                    format!(
                        "227 Entering Passive Mode (127,0,0,1,{},{})\r\n",
                        port >> 8,
                        port & 0xff
                    )
                } else {
                    format!("229 Entering Extended Passive Mode (|||{port}|)\r\n")
                }
            }
            "PASV" | "EPSV" => "425 no data connections\r\n".to_owned(),
            "LIST" | "NLST" | "MLSD" => match data.take() {
                Some(listener) => {
                    if !reply(&mut writer, "150 here it comes\r\n") {
                        return;
                    }
                    if let Ok((mut connection, _)) = listener.accept() {
                        let _ = connection.write_all(
                            b"-rw-r--r-- 1 owner group 5 Jan 01 00:00 hello.txt\r\n",
                        );
                    }
                    "226 done\r\n".to_owned()
                }
                None => "425 no data connection\r\n".to_owned(),
            },
            "QUIT" => {
                let _ = reply(&mut writer, "221 bye\r\n");
                return;
            }
            "SIZE" | "MDTM" | "STAT" | "MLST" => "550 no such file\r\n".to_owned(),
            _ => "502 not implemented\r\n".to_owned(),
        };
        if !reply(&mut writer, &answer) {
            return;
        }
    }
}

/// An in-memory secret service. It can be locked, as a desktop keyring is
/// before the user unlocks it.
#[derive(Clone, Default)]
pub(crate) struct MemoryKeyring {
    inner: Arc<Mutex<KeyringState>>,
}

#[derive(Default)]
struct KeyringState {
    locked: bool,
    secrets: BTreeMap<String, Vec<u8>>,
}

impl MemoryKeyring {
    pub(crate) fn locked() -> Self {
        let keyring = Self::default();
        keyring.state().locked = true;
        keyring
    }

    /// The secret stored under a connection ID.
    pub(crate) fn secret(&self, id: &str) -> Option<Vec<u8>> {
        self.state().secrets.get(id).cloned()
    }

    pub(crate) fn ids(&self) -> Vec<String> {
        self.state().secrets.keys().cloned().collect()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, KeyringState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn store(
        &self,
        reference: &CredentialReference,
        secret: &SecretBuffer,
        dispatch: &MutationDispatch,
    ) -> Result<(), SecretError> {
        let mut state = self.state();
        if state.locked {
            return Err(SecretError::Locked);
        }
        dispatch.mark_dispatched();
        let bytes = secret.expose_secret(<[u8]>::to_vec);
        state
            .secrets
            .insert(reference.connection_id().as_str().to_owned(), bytes);
        Ok(())
    }
}

impl SecretServiceBackend for MemoryKeyring {
    fn state(&self) -> BoxFuture<'_, Result<SecretServiceState, SecretError>> {
        let locked = self.state().locked;
        Box::pin(async move {
            Ok(if locked {
                SecretServiceState::Locked
            } else {
                SecretServiceState::Available
            })
        })
    }

    fn create<'a>(
        &'a self,
        reference: &'a CredentialReference,
        _label: &'a str,
        secret: &'a SecretBuffer,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        let result = self.store(reference, secret, dispatch);
        Box::pin(async move { result })
    }

    fn read<'a>(
        &'a self,
        reference: &'a CredentialReference,
    ) -> BoxFuture<'a, Result<SecretBuffer, SecretError>> {
        let state = self.state();
        let result = if state.locked {
            Err(SecretError::Locked)
        } else {
            state
                .secrets
                .get(reference.connection_id().as_str())
                .cloned()
                .map(SecretBuffer::new)
                .ok_or(SecretError::NotFound)
        };
        drop(state);
        Box::pin(async move { result })
    }

    fn update<'a>(
        &'a self,
        reference: &'a CredentialReference,
        _label: &'a str,
        secret: &'a SecretBuffer,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        let result = self.store(reference, secret, dispatch);
        Box::pin(async move { result })
    }

    fn delete<'a>(
        &'a self,
        reference: &'a CredentialReference,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        let mut state = self.state();
        let result = if state.locked {
            Err(SecretError::Locked)
        } else {
            dispatch.mark_dispatched();
            state
                .secrets
                .remove(reference.connection_id().as_str())
                .map(|_| ())
                .ok_or(SecretError::NotFound)
        };
        drop(state);
        Box::pin(async move { result })
    }

    fn rename<'a>(
        &'a self,
        _reference: &'a CredentialReference,
        _label: &'a str,
        _dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async { Ok(()) })
    }
}
