use super::{DeviceDescriptor, VolumeAction, VolumeId};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use zbus::blocking::{Connection, Proxy, connection::Builder};
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
}

#[derive(Clone)]
pub struct ZbusUDisksBackend {
    connection: Connection,
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
        Builder::system()
            .and_then(|builder| builder.method_timeout(timeout).build())
            .map(|connection| Self { connection })
            .map_err(map_connection_error)
    }

    /// Connect to an explicit bus address. This is primarily useful for an
    /// isolated desktop-service integration test and never shells out to a
    /// mount utility.
    pub fn connect_address_with_timeout(
        address: &str,
        timeout: Duration,
    ) -> Result<Self, UDisksError> {
        Builder::address(address)
            .and_then(|builder| builder.method_timeout(timeout).build())
            .map(|connection| Self { connection })
            .map_err(map_connection_error)
    }

    fn proxy<'a>(&'a self, path: &'a str, interface: &'a str) -> Result<Proxy<'a>, UDisksError> {
        Proxy::new(&self.connection, SERVICE, path, interface).map_err(map_zbus_error)
    }

    fn managed_objects(&self) -> Result<zbus::fdo::ManagedObjects, UDisksError> {
        let proxy = zbus::blocking::fdo::ObjectManagerProxy::builder(&self.connection)
            .destination(SERVICE)
            .and_then(|builder| builder.path(ROOT))
            .and_then(|builder| builder.build())
            .map_err(map_zbus_error)?;
        proxy.get_managed_objects().map_err(map_fdo_error)
    }

    fn service_owner(&self) -> Result<String, UDisksError> {
        let proxy =
            zbus::blocking::fdo::DBusProxy::new(&self.connection).map_err(map_zbus_error)?;
        let name = BusName::try_from(SERVICE)
            .map_err(|error| UDisksError::Protocol(error.to_string().into()))?;
        proxy
            .get_name_owner(name)
            .map(|owner| owner.to_string())
            .map_err(map_fdo_error)
    }

    fn read_device(
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
        let block = self.proxy(path, BLOCK)?;
        let mut device = block
            .get_property::<Vec<u8>>("Device")
            .map(bytes_to_path)
            .map_err(map_zbus_error)?;
        if device.as_os_str().is_empty() {
            device = PathBuf::from(path.rsplit('/').next().unwrap_or("volume"));
        }
        let uuid = block.get_property::<String>("IdUUID").unwrap_or_default();
        let label = block.get_property::<String>("IdLabel").unwrap_or_default();
        let size = block.get_property::<u64>("Size").ok();
        let read_only = block.get_property::<bool>("ReadOnly").unwrap_or(false);
        let drive_path = block.get_property::<OwnedObjectPath>("Drive").ok();
        let symlinks = block
            .get_property::<Vec<Vec<u8>>>("Symlinks")
            .unwrap_or_default();
        let has_filesystem = interfaces.keys().any(|name| name.as_str() == FILESYSTEM);
        let mount_points = self.mount_points(path, has_filesystem)?;
        let has_encrypted = interfaces.keys().any(|name| name.as_str() == ENCRYPTED);
        let locked = self.is_locked(path, has_encrypted)?;
        let (can_eject, can_power_off, drive_identity) = self.drive_facts(drive_path.as_ref());
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

    fn mount_points(&self, path: &str, present: bool) -> Result<Vec<PathBuf>, UDisksError> {
        if !present {
            return Ok(Vec::new());
        }
        self.proxy(path, FILESYSTEM)?
            .get_property::<Vec<Vec<u8>>>("MountPoints")
            .map(|points| points.into_iter().map(bytes_to_path).collect())
            .map_err(map_zbus_error)
    }

    fn is_locked(&self, path: &str, encrypted: bool) -> Result<bool, UDisksError> {
        if !encrypted {
            return Ok(false);
        }
        Ok(self
            .proxy(path, ENCRYPTED)?
            .get_property::<OwnedObjectPath>("CleartextDevice")
            .map(|cleartext| cleartext.as_str() == "/")
            .unwrap_or(true))
    }

    fn drive_facts(&self, path: Option<&OwnedObjectPath>) -> (bool, bool, String) {
        path.filter(|drive| drive.as_str() != "/")
            .and_then(|drive| self.proxy(drive.as_str(), DRIVE).ok())
            .map_or((false, false, String::new()), |drive| {
                (
                    drive.get_property::<bool>("Ejectable").unwrap_or(false),
                    drive.get_property::<bool>("CanPowerOff").unwrap_or(false),
                    drive
                        .get_property::<String>("WWN")
                        .ok()
                        .filter(|value| !value.is_empty())
                        .or_else(|| drive.get_property::<String>("Serial").ok())
                        .unwrap_or_default(),
                )
            })
    }
}

impl UDisksBackend for ZbusUDisksBackend {
    fn snapshot(&self) -> Result<BackendSnapshot, UDisksError> {
        let owner = self.service_owner()?;
        let objects = self.managed_objects()?;
        let mut devices = Vec::new();
        for (path, interfaces) in objects {
            if let Some(device) = self.read_device(path.as_str(), &interfaces)? {
                devices.push(device);
            }
        }
        Ok(BackendSnapshot::new(owner, devices))
    }

    fn perform(
        &self,
        volume: &DeviceDescriptor,
        action: VolumeAction,
        unlock_secret: Option<&str>,
    ) -> Result<(), UDisksError> {
        let options: HashMap<&str, Value<'_>> = HashMap::new();
        match action {
            VolumeAction::Mount => {
                let _: String = self
                    .proxy(volume.object_path(), FILESYSTEM)?
                    .call("Mount", &(options,))
                    .map_err(map_zbus_error)?;
            }
            VolumeAction::Unmount => {
                let _: () = self
                    .proxy(volume.object_path(), FILESYSTEM)?
                    .call("Unmount", &(options,))
                    .map_err(map_zbus_error)?;
            }
            VolumeAction::Eject => {
                let drive = volume.drive_path().ok_or_else(|| {
                    UDisksError::Unsupported("the volume has no drive object".into())
                })?;
                let _: () = self
                    .proxy(drive, DRIVE)?
                    .call("Eject", &(options,))
                    .map_err(map_zbus_error)?;
            }
            VolumeAction::Unlock => {
                let secret = unlock_secret.ok_or_else(|| {
                    UDisksError::AuthorizationRequired("an unlock secret is required".into())
                })?;
                let _: OwnedObjectPath = self
                    .proxy(volume.object_path(), ENCRYPTED)?
                    .call("Unlock", &(secret, options))
                    .map_err(map_zbus_error)?;
            }
            VolumeAction::PowerOff => {
                let drive = volume.drive_path().ok_or_else(|| {
                    UDisksError::Unsupported("the volume has no drive object".into())
                })?;
                let _: () = self
                    .proxy(drive, DRIVE)?
                    .call("PowerOff", &(options,))
                    .map_err(map_zbus_error)?;
            }
        }
        Ok(())
    }
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
    bytes.extend_from_slice(drive.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(device.as_os_str().as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(object.as_bytes());
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
        if name.contains("NotAuthorized") || name.contains("Auth") {
            return UDisksError::AuthorizationRequired(detail.into());
        }
        if name.contains("DeviceBusy")
            || name.contains("Busy")
            || detail.contains("DeviceBusy")
            || detail.contains("Busy")
        {
            return UDisksError::Busy(detail.into());
        }
        if name.contains("NotSupported") || name.contains("Unsupported") {
            return UDisksError::Unsupported(detail.into());
        }
        if name.contains("UnknownObject") || name.contains("UnknownMethod") {
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

pub(crate) fn spawn_udisks_event_listener(
    sender: async_channel::Sender<super::VolumeTrigger>,
) -> Vec<thread::JoinHandle<()>> {
    let mut threads = Vec::new();
    if let Some(thread) = spawn_name_owner_listener(sender.clone()) {
        threads.push(thread);
    }
    if let Ok(thread) = thread::Builder::new()
        .name("musheen-udisks-events".into())
        .spawn(move || {
            while !sender.is_closed() {
                if futures_lite::future::block_on(listen_for_udisks_events(&sender)).is_err() {
                    let _ = sender.try_send(super::VolumeTrigger::ServiceOwnerChanged);
                }
                if sender.is_closed() {
                    break;
                }
                // This delay only bounds reconnect attempts after a broken bus;
                // volume changes themselves are delivered by D-Bus signals.
                thread::sleep(Duration::from_secs(1));
            }
        })
    {
        threads.push(thread);
    }
    threads
}

fn spawn_name_owner_listener(
    sender: async_channel::Sender<super::VolumeTrigger>,
) -> Option<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("musheen-udisks-owner-events".into())
        .spawn(move || {
            while !sender.is_closed() {
                let result = futures_lite::future::block_on(async {
                    use futures_lite::StreamExt as _;
                    use zbus::message::Type;

                    let connection = zbus::Connection::system()
                        .await
                        .map_err(map_connection_error)?;
                    let rule = zbus::MatchRule::builder()
                        .msg_type(Type::Signal)
                        .interface("org.freedesktop.DBus")
                        .map_err(map_zbus_error)?
                        .member("NameOwnerChanged")
                        .map_err(map_zbus_error)?
                        .add_arg(SERVICE)
                        .map_err(map_zbus_error)?
                        .build();
                    let mut messages =
                        zbus::MessageStream::for_match_rule(rule, &connection, Some(8))
                            .await
                            .map_err(map_zbus_error)?;
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
                    thread::sleep(Duration::from_secs(1));
                }
            }
        })
        .ok()
}

async fn listen_for_udisks_events(
    sender: &async_channel::Sender<super::VolumeTrigger>,
) -> Result<(), UDisksError> {
    use futures_lite::StreamExt as _;
    use zbus::message::Type;

    let connection = zbus::Connection::system()
        .await
        .map_err(map_connection_error)?;
    let rule = zbus::MatchRule::builder()
        .msg_type(Type::Signal)
        .path_namespace(ROOT)
        .map_err(map_zbus_error)?
        .build();
    let mut messages = zbus::MessageStream::for_match_rule(rule, &connection, Some(32))
        .await
        .map_err(map_zbus_error)?;
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
    fn io_deadlines_map_to_typed_timeouts() {
        let error = zbus::Error::InputOutput(Arc::new(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "deadline exceeded",
        )));
        assert!(matches!(map_zbus_error(error), UDisksError::Timeout(_)));
    }
}
