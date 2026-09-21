use super::{DeviceDescriptor, VolumeAction, VolumeId};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::future::Future;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use zbus::names::BusName;
use zbus::zvariant::{OwnedObjectPath, Value};

const SERVICE: &str = "org.freedesktop.UDisks2";
const ROOT: &str = "/org/freedesktop/UDisks2";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";
const ENCRYPTED: &str = "org.freedesktop.UDisks2.Encrypted";
const DEFAULT_METHOD_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UDisksBusConfig {
    address: Option<Box<str>>,
    service: Box<str>,
    root: Box<str>,
}

impl UDisksBusConfig {
    #[must_use]
    pub fn system() -> Self {
        Self {
            address: None,
            service: SERVICE.into(),
            root: ROOT.into(),
        }
    }

    #[must_use]
    pub fn address(address: impl Into<Box<str>>) -> Self {
        Self {
            address: Some(address.into()),
            ..Self::system()
        }
    }

    fn service(&self) -> &str {
        &self.service
    }

    fn root(&self) -> &str {
        &self.root
    }
}

#[derive(Clone, Debug)]
pub struct UDisksRequest {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl UDisksRequest {
    #[must_use]
    pub fn with_timeout(timeout: Duration) -> Self {
        Self::with_cancel(timeout, Arc::new(AtomicBool::new(false)))
    }

    #[must_use]
    pub(crate) fn with_cancel(timeout: Duration, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            cancelled,
        }
    }

    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    pub fn check(&self) -> Result<(), UDisksError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(UDisksError::WorkerStopped);
        }
        if Instant::now() >= self.deadline {
            return Err(UDisksError::DeadlineExceeded);
        }
        Ok(())
    }

    async fn expiry(&self) -> UDisksError {
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return UDisksError::WorkerStopped;
            }
            let remaining = self.remaining();
            if remaining.is_zero() {
                return UDisksError::DeadlineExceeded;
            }
            async_io::Timer::after(remaining.min(Duration::from_millis(20))).await;
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendSnapshot {
    owner: Box<str>,
    devices: Vec<DeviceDescriptor>,
}

impl BackendSnapshot {
    #[must_use]
    pub fn new(
        owner: impl Into<Box<str>>,
        devices: impl IntoIterator<Item = DeviceDescriptor>,
    ) -> Self {
        let mut devices = devices.into_iter().collect::<Vec<_>>();
        devices.sort_by(|left, right| left.id().cmp(right.id()));
        Self {
            owner: owner.into(),
            devices,
        }
    }

    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    #[must_use]
    pub fn devices(&self) -> &[DeviceDescriptor] {
        &self.devices
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UDisksError {
    Unavailable(Box<str>),
    Timeout(Box<str>),
    Disconnected(Box<str>),
    AuthorizationRequired(Box<str>),
    Busy(Box<str>),
    Unsupported(Box<str>),
    DeadlineExceeded,
    WorkerStopped,
    UnlockSecretRequired,
    ActionUnavailable,
    StaleObject,
    Protocol(Box<str>),
}

impl fmt::Display for UDisksError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(formatter, "UDisks2 is unavailable: {reason}"),
            Self::Timeout(reason) => write!(formatter, "UDisks2 timed out: {reason}"),
            Self::Disconnected(reason) => {
                write!(formatter, "the system bus disconnected: {reason}")
            }
            Self::AuthorizationRequired(reason) => {
                write!(formatter, "authorization is required: {reason}")
            }
            Self::Busy(reason) => write!(formatter, "the volume is busy: {reason}"),
            Self::Unsupported(reason) => {
                write!(formatter, "the volume action is unsupported: {reason}")
            }
            Self::DeadlineExceeded => formatter.write_str("operation deadline exceeded"),
            Self::WorkerStopped => formatter.write_str("the volume worker stopped"),
            Self::UnlockSecretRequired => formatter.write_str("an unlock secret is required"),
            Self::ActionUnavailable => formatter.write_str("UDisks2 did not advertise this action"),
            Self::StaleObject => formatter.write_str("the UDisks2 object no longer exists"),
            Self::Protocol(reason) => write!(formatter, "invalid UDisks2 response: {reason}"),
        }
    }
}

impl std::error::Error for UDisksError {}

pub trait UDisksBackend: Send + Sync {
    fn snapshot(&self) -> Result<BackendSnapshot, UDisksError>;
    fn perform(
        &self,
        volume: &DeviceDescriptor,
        action: VolumeAction,
        unlock_secret: Option<&str>,
    ) -> Result<(), UDisksError>;

    fn snapshot_with_request(
        &self,
        request: &UDisksRequest,
    ) -> Result<BackendSnapshot, UDisksError> {
        request.check()?;
        let result = self.snapshot();
        request.check()?;
        result
    }

    fn perform_with_request(
        &self,
        volume: &DeviceDescriptor,
        action: VolumeAction,
        unlock_secret: Option<&str>,
        request: &UDisksRequest,
    ) -> Result<(), UDisksError> {
        request.check()?;
        let result = self.perform(volume, action, unlock_secret);
        request.check()?;
        result
    }
}

#[derive(Clone)]
pub struct ZbusUDisksBackend {
    connection: zbus::Connection,
    config: UDisksBusConfig,
    default_timeout: Duration,
}

impl fmt::Debug for ZbusUDisksBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ZbusUDisksBackend")
            .finish_non_exhaustive()
    }
}

impl ZbusUDisksBackend {
    pub fn connect_system() -> Result<Self, UDisksError> {
        Self::connect_system_with_timeout(DEFAULT_METHOD_TIMEOUT)
    }

    pub fn connect_system_with_timeout(timeout: Duration) -> Result<Self, UDisksError> {
        Self::connect(UDisksBusConfig::system(), timeout)
    }

    /// Connect to an explicit bus address. This is primarily useful for an
    /// isolated desktop-service integration test and never shells out to a
    /// mount utility.
    pub fn connect_address_with_timeout(
        address: &str,
        timeout: Duration,
    ) -> Result<Self, UDisksError> {
        Self::connect(UDisksBusConfig::address(address), timeout)
    }

    pub fn connect(config: UDisksBusConfig, timeout: Duration) -> Result<Self, UDisksError> {
        let request = UDisksRequest::with_timeout(timeout);
        let builder = match config.address.as_deref() {
            Some(address) => zbus::connection::Builder::address(address),
            None => zbus::connection::Builder::system(),
        }
        .map_err(map_connection_error)?;
        let connection = block_on_request(&request, async move {
            builder.build().await.map_err(map_connection_error)
        })?;
        Ok(Self {
            connection,
            config,
            default_timeout: timeout,
        })
    }

    async fn proxy<'a>(
        &'a self,
        path: &'a str,
        interface: &'a str,
    ) -> Result<zbus::Proxy<'a>, UDisksError> {
        zbus::Proxy::new(&self.connection, self.config.service(), path, interface)
            .await
            .map_err(map_zbus_error)
    }

    async fn managed_objects(&self) -> Result<zbus::fdo::ManagedObjects, UDisksError> {
        let proxy = zbus::fdo::ObjectManagerProxy::builder(&self.connection)
            .destination(self.config.service())
            .map_err(map_zbus_error)?
            .path(self.config.root())
            .map_err(map_zbus_error)?
            .build()
            .await
            .map_err(map_zbus_error)?;
        proxy.get_managed_objects().await.map_err(map_fdo_error)
    }

    async fn service_owner(&self) -> Result<String, UDisksError> {
        let proxy = zbus::fdo::DBusProxy::new(&self.connection)
            .await
            .map_err(map_zbus_error)?;
        let name = BusName::try_from(self.config.service())
            .map_err(|error| UDisksError::Protocol(error.to_string().into()))?;
        proxy
            .get_name_owner(name)
            .await
            .map(|owner| owner.to_string())
            .map_err(map_fdo_error)
    }

    async fn read_device(
        &self,
        path: &str,
        interfaces: &HashMap<
            zbus::names::OwnedInterfaceName,
            HashMap<String, zbus::zvariant::OwnedValue>,
        >,
    ) -> Result<Option<DeviceDescriptor>, UDisksError> {
        if !interfaces.keys().any(|name| name.as_str() == BLOCK) {
            return Ok(None);
        }
        let block = self.proxy(path, BLOCK).await?;
        let mut device = block
            .get_property::<Vec<u8>>("Device")
            .await
            .map(bytes_to_path)
            .map_err(map_zbus_error)?;
        if device.as_os_str().is_empty() {
            device = PathBuf::from(path.rsplit('/').next().unwrap_or("volume"));
        }
        let uuid = block
            .get_property::<String>("IdUUID")
            .await
            .unwrap_or_default();
        let label = block
            .get_property::<String>("IdLabel")
            .await
            .unwrap_or_default();
        let size = block.get_property::<u64>("Size").await.ok();
        let read_only = block
            .get_property::<bool>("ReadOnly")
            .await
            .unwrap_or(false);
        let drive_path = block.get_property::<OwnedObjectPath>("Drive").await.ok();
        let symlinks = block
            .get_property::<Vec<Vec<u8>>>("Symlinks")
            .await
            .unwrap_or_default();
        let has_filesystem = interfaces.keys().any(|name| name.as_str() == FILESYSTEM);
        let mount_points = self.mount_points(path, has_filesystem).await?;
        let has_encrypted = interfaces.keys().any(|name| name.as_str() == ENCRYPTED);
        let locked = self.is_locked(path, has_encrypted).await?;
        let (can_eject, can_power_off, drive_identity) =
            self.drive_facts(drive_path.as_ref()).await;
        let persistent_hint = symlinks
            .iter()
            .map(|bytes| bytes_to_path(bytes.clone()))
            .find(|path| path.starts_with("/dev/disk/by-id"))
            .unwrap_or_else(|| device.clone());
        // UUIDs are filesystem identifiers, not device identifiers: cloned
        // media can legitimately share them. The hardware hint provides
        // persistence while the object path is the collision discriminator.
        let id_value = stable_device_id(&uuid, &drive_identity, &persistent_hint, path);
        let id = VolumeId::new(id_value)
            .map_err(|error| UDisksError::Protocol(error.to_string().into()))?;
        let display_label = if label.is_empty() {
            device
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| id.as_str().to_owned())
        } else {
            label
        };
        let mounted = !mount_points.is_empty();
        let mut descriptor = DeviceDescriptor::new(id, path)
            .with_label(display_label)
            .with_device(device)
            .with_mount_points(mount_points)
            .with_capabilities(
                has_filesystem && !locked && !mounted,
                has_filesystem && !locked && mounted,
                can_eject,
                has_encrypted && locked,
                can_power_off,
            )
            .with_read_only(read_only)
            .with_locked(locked);
        if let Some(path) = drive_path.filter(|path| path.as_str() != "/") {
            descriptor = descriptor.with_drive_path(path.to_string());
        }
        if let Some(size) = size {
            descriptor = descriptor.with_size_bytes(size);
        }
        Ok(Some(descriptor))
    }

    async fn mount_points(&self, path: &str, present: bool) -> Result<Vec<PathBuf>, UDisksError> {
        if !present {
            return Ok(Vec::new());
        }
        self.proxy(path, FILESYSTEM)
            .await?
            .get_property::<Vec<Vec<u8>>>("MountPoints")
            .await
            .map(|points| points.into_iter().map(bytes_to_path).collect())
            .map_err(map_zbus_error)
    }

    async fn is_locked(&self, path: &str, encrypted: bool) -> Result<bool, UDisksError> {
        if !encrypted {
            return Ok(false);
        }
        Ok(self
            .proxy(path, ENCRYPTED)
            .await?
            .get_property::<OwnedObjectPath>("CleartextDevice")
            .await
            .map(|cleartext| cleartext.as_str() == "/")
            .unwrap_or(true))
    }

    async fn drive_facts(&self, path: Option<&OwnedObjectPath>) -> (bool, bool, String) {
        let Some(path) = path.filter(|drive| drive.as_str() != "/") else {
            return (false, false, String::new());
        };
        let Ok(drive) = self.proxy(path.as_str(), DRIVE).await else {
            return (false, false, String::new());
        };
        let ejectable = drive
            .get_property::<bool>("Ejectable")
            .await
            .unwrap_or(false);
        let can_power_off = drive
            .get_property::<bool>("CanPowerOff")
            .await
            .unwrap_or(false);
        let mut identity = drive
            .get_property::<String>("WWN")
            .await
            .unwrap_or_default();
        if identity.is_empty() {
            identity = drive
                .get_property::<String>("Serial")
                .await
                .unwrap_or_default();
        }
        (ejectable, can_power_off, identity)
    }
}

impl UDisksBackend for ZbusUDisksBackend {
    fn snapshot(&self) -> Result<BackendSnapshot, UDisksError> {
        self.snapshot_with_request(&UDisksRequest::with_timeout(self.default_timeout))
    }

    fn perform(
        &self,
        volume: &DeviceDescriptor,
        action: VolumeAction,
        unlock_secret: Option<&str>,
    ) -> Result<(), UDisksError> {
        self.perform_with_request(
            volume,
            action,
            unlock_secret,
            &UDisksRequest::with_timeout(self.default_timeout),
        )
    }

    fn snapshot_with_request(
        &self,
        request: &UDisksRequest,
    ) -> Result<BackendSnapshot, UDisksError> {
        block_on_request(request, async {
            let owner = self.service_owner().await?;
            let objects = self.managed_objects().await?;
            let mut devices = Vec::new();
            for (path, interfaces) in objects {
                if let Some(device) = self.read_device(path.as_str(), &interfaces).await? {
                    devices.push(device);
                }
            }
            Ok(BackendSnapshot::new(owner, devices))
        })
    }

    fn perform_with_request(
        &self,
        volume: &DeviceDescriptor,
        action: VolumeAction,
        unlock_secret: Option<&str>,
        request: &UDisksRequest,
    ) -> Result<(), UDisksError> {
        block_on_request(request, async {
            let options: HashMap<&str, Value<'_>> = HashMap::new();
            match action {
                VolumeAction::Mount => {
                    let _: String = self
                        .proxy(volume.object_path(), FILESYSTEM)
                        .await?
                        .call("Mount", &(options,))
                        .await
                        .map_err(map_zbus_error)?;
                }
                VolumeAction::Unmount => {
                    let _: () = self
                        .proxy(volume.object_path(), FILESYSTEM)
                        .await?
                        .call("Unmount", &(options,))
                        .await
                        .map_err(map_zbus_error)?;
                }
                VolumeAction::Eject => {
                    let drive = volume.drive_path().ok_or(UDisksError::ActionUnavailable)?;
                    let _: () = self
                        .proxy(drive, DRIVE)
                        .await?
                        .call("Eject", &(options,))
                        .await
                        .map_err(map_zbus_error)?;
                }
                VolumeAction::Unlock => {
                    let secret = unlock_secret.ok_or(UDisksError::UnlockSecretRequired)?;
                    let _: OwnedObjectPath = self
                        .proxy(volume.object_path(), ENCRYPTED)
                        .await?
                        .call("Unlock", &(secret, options))
                        .await
                        .map_err(map_zbus_error)?;
                }
                VolumeAction::PowerOff => {
                    let drive = volume.drive_path().ok_or(UDisksError::ActionUnavailable)?;
                    let _: () = self
                        .proxy(drive, DRIVE)
                        .await?
                        .call("PowerOff", &(options,))
                        .await
                        .map_err(map_zbus_error)?;
                }
            }
            Ok(())
        })
    }
}

fn block_on_request<T>(
    request: &UDisksRequest,
    operation: impl Future<Output = Result<T, UDisksError>>,
) -> Result<T, UDisksError> {
    request.check()?;
    futures_lite::future::block_on(futures_lite::future::race(operation, async {
        Err(request.expiry().await)
    }))
}

fn bytes_to_path(mut bytes: Vec<u8>) -> PathBuf {
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    PathBuf::from(OsString::from_vec(bytes))
}

fn stable_device_id(uuid: &str, drive: &str, device: &std::path::Path, object: &str) -> String {
    use std::os::unix::ffi::OsStrExt as _;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(uuid.as_bytes());
    bytes.push(0);
    // A by-id symlink, WWN, or serial survives reconnects, kernel device-name
    // changes, and UDisks object recreation. Only truly anonymous devices need
    // the volatile object path as a last-resort collision discriminator.
    if !drive.is_empty() {
        bytes.extend_from_slice(drive.as_bytes());
    } else {
        bytes.extend_from_slice(device.as_os_str().as_bytes());
    }
    if drive.is_empty() && !device.starts_with("/dev/disk/by-id") {
        bytes.push(0);
        bytes.extend_from_slice(object.as_bytes());
    }
    let mut encoded = String::with_capacity(bytes.len() * 2 + 7);
    encoded.push_str("device-");
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn map_connection_error(error: zbus::Error) -> UDisksError {
    match error {
        zbus::Error::InputOutput(ref io) if io.kind() == std::io::ErrorKind::TimedOut => {
            UDisksError::Timeout(error.to_string().into())
        }
        zbus::Error::Connection(_, _) | zbus::Error::InputOutput(_) => {
            UDisksError::Disconnected(error.to_string().into())
        }
        _ => UDisksError::Unavailable(error.to_string().into()),
    }
}

fn map_fdo_error(error: zbus::fdo::Error) -> UDisksError {
    match error {
        zbus::fdo::Error::ZBus(error) => map_zbus_error(error),
        zbus::fdo::Error::NoReply(reason)
        | zbus::fdo::Error::Timeout(reason)
        | zbus::fdo::Error::TimedOut(reason) => UDisksError::Timeout(reason.into()),
        zbus::fdo::Error::Disconnected(reason) | zbus::fdo::Error::IOError(reason) => {
            UDisksError::Disconnected(reason.into())
        }
        zbus::fdo::Error::ServiceUnknown(reason) | zbus::fdo::Error::NameHasNoOwner(reason) => {
            UDisksError::Unavailable(reason.into())
        }
        other => UDisksError::Protocol(other.to_string().into()),
    }
}

fn map_zbus_error(error: zbus::Error) -> UDisksError {
    if let zbus::Error::MethodError(name, detail, _) = &error {
        let name = name.as_str();
        let detail = detail.as_deref().unwrap_or(name);
        if name.contains("NotAuthorized")
            || name.contains("Auth")
            || detail.contains("NotAuthorized")
            || detail.contains("Auth")
        {
            return UDisksError::AuthorizationRequired(detail.into());
        }
        if name.contains("DeviceBusy")
            || name.contains("Busy")
            || detail.contains("DeviceBusy")
            || detail.contains("Busy")
        {
            return UDisksError::Busy(detail.into());
        }
        if name.contains("NotSupported")
            || name.contains("Unsupported")
            || detail.contains("NotSupported")
            || detail.contains("Unsupported")
        {
            return UDisksError::Unsupported(detail.into());
        }
        if name.contains("UnknownObject")
            || name.contains("UnknownMethod")
            || detail.contains("UnknownObject")
            || detail.contains("UnknownMethod")
        {
            return UDisksError::StaleObject;
        }
    }
    match error {
        zbus::Error::InputOutput(ref io) if io.kind() == std::io::ErrorKind::TimedOut => {
            UDisksError::Timeout(error.to_string().into())
        }
        zbus::Error::Connection(_, _) | zbus::Error::InputOutput(_) => {
            UDisksError::Disconnected(error.to_string().into())
        }
        zbus::Error::FDO(error) => map_fdo_error(*error),
        _ => UDisksError::Protocol(error.to_string().into()),
    }
}

pub(crate) fn spawn_udisks_event_listener_with_config(
    sender: async_channel::Sender<super::VolumeTrigger>,
    config: UDisksBusConfig,
) -> Vec<thread::JoinHandle<()>> {
    let mut threads = Vec::new();
    if let Some(thread) = spawn_name_owner_listener(sender.clone(), config.clone()) {
        threads.push(thread);
    }
    if let Ok(thread) = thread::Builder::new()
        .name("musheen-udisks-events".into())
        .spawn(move || {
            while !sender.is_closed() {
                if futures_lite::future::block_on(listen_for_udisks_events(&sender, &config))
                    .is_err()
                {
                    let _ = sender.try_send(super::VolumeTrigger::ServiceOwnerChanged);
                }
                if sender.is_closed() {
                    break;
                }
                // This delay only bounds reconnect attempts after a broken bus;
                // volume changes themselves are delivered by D-Bus signals.
                wait_for_reconnect_or_shutdown(&sender);
            }
        })
    {
        threads.push(thread);
    }
    threads
}

fn spawn_name_owner_listener(
    sender: async_channel::Sender<super::VolumeTrigger>,
    config: UDisksBusConfig,
) -> Option<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("musheen-udisks-owner-events".into())
        .spawn(move || {
            while !sender.is_closed() {
                let result = futures_lite::future::block_on(async {
                    use futures_lite::StreamExt as _;
                    use zbus::message::Type;

                    let Some(connection) =
                        until_listener_shutdown(&sender, connect_async(&config)).await?
                    else {
                        return Ok::<(), UDisksError>(());
                    };
                    let rule = zbus::MatchRule::builder()
                        .msg_type(Type::Signal)
                        .interface("org.freedesktop.DBus")
                        .map_err(map_zbus_error)?
                        .member("NameOwnerChanged")
                        .map_err(map_zbus_error)?
                        .add_arg(config.service())
                        .map_err(map_zbus_error)?
                        .build();
                    let Some(mut messages) = until_listener_shutdown(&sender, async {
                        zbus::MessageStream::for_match_rule(rule, &connection, Some(8))
                            .await
                            .map_err(map_zbus_error)
                    })
                    .await?
                    else {
                        return Ok::<(), UDisksError>(());
                    };
                    loop {
                        let message =
                            futures_lite::future::race(async { messages.next().await }, async {
                                sender.closed().await;
                                None
                            })
                            .await;
                        let Some(message) = message else {
                            return Ok::<(), UDisksError>(());
                        };
                        message.map_err(map_zbus_error)?;
                        if sender
                            .try_send(super::VolumeTrigger::ServiceOwnerChanged)
                            .is_err()
                            && sender.is_closed()
                        {
                            return Ok::<(), UDisksError>(());
                        }
                    }
                });
                if result.is_err() {
                    let _ = sender.try_send(super::VolumeTrigger::ServiceOwnerChanged);
                }
                if !sender.is_closed() {
                    wait_for_reconnect_or_shutdown(&sender);
                }
            }
        })
        .ok()
}

async fn listen_for_udisks_events(
    sender: &async_channel::Sender<super::VolumeTrigger>,
    config: &UDisksBusConfig,
) -> Result<(), UDisksError> {
    use futures_lite::StreamExt as _;
    use zbus::message::Type;

    let Some(connection) = until_listener_shutdown(sender, connect_async(config)).await? else {
        return Ok(());
    };
    let rule = zbus::MatchRule::builder()
        .msg_type(Type::Signal)
        .path_namespace(config.root())
        .map_err(map_zbus_error)?
        .build();
    let Some(mut messages) = until_listener_shutdown(sender, async {
        zbus::MessageStream::for_match_rule(rule, &connection, Some(32))
            .await
            .map_err(map_zbus_error)
    })
    .await?
    else {
        return Ok(());
    };
    loop {
        let message = futures_lite::future::race(async { messages.next().await }, async {
            sender.closed().await;
            None
        })
        .await;
        let Some(message) = message else {
            return Ok(());
        };
        message.map_err(map_zbus_error)?;
        if sender
            .try_send(super::VolumeTrigger::UDisksChanged)
            .is_err()
            && sender.is_closed()
        {
            return Ok(());
        }
    }
}

async fn until_listener_shutdown<T>(
    sender: &async_channel::Sender<super::VolumeTrigger>,
    operation: impl Future<Output = Result<T, UDisksError>>,
) -> Result<Option<T>, UDisksError> {
    futures_lite::future::race(async { operation.await.map(Some) }, async {
        sender.closed().await;
        Ok(None)
    })
    .await
}

fn wait_for_reconnect_or_shutdown(sender: &async_channel::Sender<super::VolumeTrigger>) {
    futures_lite::future::block_on(futures_lite::future::race(
        async { sender.closed().await },
        async {
            async_io::Timer::after(Duration::from_secs(1)).await;
        },
    ));
}

async fn connect_async(config: &UDisksBusConfig) -> Result<zbus::Connection, UDisksError> {
    let builder = match config.address.as_deref() {
        Some(address) => zbus::connection::Builder::address(address),
        None => zbus::connection::Builder::system(),
    }
    .map_err(map_connection_error)?;
    builder.build().await.map_err(map_connection_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn cloned_filesystem_uuids_have_distinct_stable_device_ids() {
        let first = stable_device_id(
            "same-uuid",
            "SERIAL-A",
            std::path::Path::new("/dev/disk/by-id/a"),
            "/org/freedesktop/UDisks2/block_devices/sdb1",
        );
        let second = stable_device_id(
            "same-uuid",
            "SERIAL-B",
            std::path::Path::new("/dev/disk/by-id/b"),
            "/org/freedesktop/UDisks2/block_devices/sdc1",
        );
        assert_ne!(first, second);
        assert_eq!(
            first,
            stable_device_id(
                "same-uuid",
                "SERIAL-A",
                std::path::Path::new("/dev/disk/by-id/a"),
                "/org/freedesktop/UDisks2/block_devices/sdb1",
            )
        );
    }

    #[test]
    fn persistent_hardware_identity_survives_object_and_kernel_path_changes() {
        let before = stable_device_id(
            "same-uuid",
            "wwn-123",
            std::path::Path::new("/dev/sdb1"),
            "/org/freedesktop/UDisks2/block_devices/sdb1",
        );
        let after = stable_device_id(
            "same-uuid",
            "wwn-123",
            std::path::Path::new("/dev/sdz1"),
            "/org/freedesktop/UDisks2/block_devices/sdz1",
        );
        assert_eq!(before, after);
    }

    #[test]
    fn io_deadlines_map_to_typed_timeouts() {
        let error = zbus::Error::InputOutput(Arc::new(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "deadline exceeded",
        )));
        assert!(matches!(map_zbus_error(error), UDisksError::Timeout(_)));
    }
}
