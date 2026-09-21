use super::{DeviceDescriptor, VolumeAction, VolumeId};
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
const PARTITION: &str = "org.freedesktop.UDisks2.Partition";
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

    fn validate_action(
        &self,
        volume: &DeviceDescriptor,
        _expected_owner: Option<&str>,
        _action: VolumeAction,
        request: &UDisksRequest,
    ) -> Result<Vec<PathBuf>, UDisksError> {
        request.check()?;
        Ok(volume.mount_points().to_vec())
    }

    fn perform_validated_with_request(
        &self,
        volume: &DeviceDescriptor,
        _expected_owner: Option<&str>,
        action: VolumeAction,
        unlock_secret: Option<&str>,
        request: &UDisksRequest,
    ) -> Result<(), UDisksError> {
        self.perform_with_request(volume, action, unlock_secret, request)
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
        self.proxy_at(self.config.service(), path, interface).await
    }

    async fn proxy_at<'a>(
        &'a self,
        destination: &'a str,
        path: &'a str,
        interface: &'a str,
    ) -> Result<zbus::Proxy<'a>, UDisksError> {
        zbus::Proxy::new(&self.connection, destination, path, interface)
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
    ) -> Result<Option<ObservedDevice>, UDisksError> {
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
        let has_partition = interfaces.keys().any(|name| name.as_str() == PARTITION);
        let locked = self.is_locked(path, has_encrypted).await?;
        let (can_eject, can_power_off, drive_identity) =
            self.drive_facts(drive_path.as_ref()).await;
        let identity = self
            .device_identity(
                path,
                &uuid,
                &drive_identity,
                &device,
                &symlinks,
                has_partition,
            )
            .await;
        let identity_stable = identity.stable;
        let id = VolumeId::new(identity.value)
            .map_err(|error| UDisksError::Protocol(error.to_string().into()))?;
        let fallback_id = ambiguous_device_id(&uuid, &device, path);
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
                identity_stable && has_filesystem && !locked && !mounted,
                identity_stable && has_filesystem && !locked && mounted,
                identity_stable && can_eject,
                identity_stable && has_encrypted && locked,
                identity_stable && can_power_off,
            )
            .with_read_only(read_only)
            .with_locked(locked);
        if let Some(path) = drive_path.as_ref().filter(|path| path.as_str() != "/") {
            descriptor = descriptor.with_drive_path(path.to_string());
        }
        if let Some(size) = size {
            descriptor = descriptor.with_size_bytes(size);
        }
        Ok(Some(ObservedDevice {
            descriptor,
            fallback_id,
            hardware_identity: identity.hardware_identity,
            drive_object: drive_path
                .as_ref()
                .filter(|path| path.as_str() != "/")
                .map(|path| path.to_string().into_boxed_str()),
        }))
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

    async fn partition_identity(&self, path: &str, present: bool) -> Option<String> {
        if !present {
            return None;
        }
        let partition = self.proxy(path, PARTITION).await.ok()?;
        let uuid = partition
            .get_property::<String>("UUID")
            .await
            .unwrap_or_default();
        let number = partition.get_property::<u32>("Number").await.ok()?;
        let offset = partition.get_property::<u64>("Offset").await.ok()?;
        Some(format!("uuid:{uuid}:number:{number}:offset:{offset}"))
    }

    async fn device_identity(
        &self,
        path: &str,
        uuid: &str,
        drive: &str,
        device: &std::path::Path,
        symlinks: &[Vec<u8>],
        partition: bool,
    ) -> DeviceIdentity {
        let stable_link = symlinks
            .iter()
            .map(|bytes| bytes_to_path(bytes.clone()))
            .filter(|path| is_stable_block_link(path))
            .min();
        let partition_hint = self.partition_identity(path, partition).await;
        let fallback = if partition || drive.is_empty() {
            BlockIdentity::Ambiguous
        } else {
            BlockIdentity::WholeDevice
        };
        let block = stable_link
            .as_deref()
            .map(BlockIdentity::Link)
            .or_else(|| partition_hint.as_deref().map(BlockIdentity::Partition))
            .unwrap_or(fallback);
        stable_device_id(uuid, drive, block, device, path)
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
            Ok(BackendSnapshot::new(
                owner,
                reconcile_device_identities(devices),
            ))
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

    fn validate_action(
        &self,
        volume: &DeviceDescriptor,
        expected_owner: Option<&str>,
        action: VolumeAction,
        request: &UDisksRequest,
    ) -> Result<Vec<PathBuf>, UDisksError> {
        let snapshot = self.snapshot_with_request(request)?;
        if expected_owner.is_none_or(|owner| owner != snapshot.owner()) {
            return Err(UDisksError::StaleObject);
        }
        let current = snapshot
            .devices()
            .iter()
            .find(|candidate| candidate.id() == volume.id())
            .ok_or(UDisksError::StaleObject)?;
        if current.object_path() != volume.object_path()
            || current.device() != volume.device()
            || current.drive_path() != volume.drive_path()
        {
            return Err(UDisksError::StaleObject);
        }
        let drive = if matches!(action, VolumeAction::Eject | VolumeAction::PowerOff) {
            Some(current.drive_path().ok_or(UDisksError::ActionUnavailable)?)
        } else {
            None
        };
        Ok(snapshot
            .devices()
            .iter()
            .filter(|candidate| {
                if matches!(action, VolumeAction::Eject | VolumeAction::PowerOff) {
                    candidate.drive_path() == drive
                } else {
                    candidate.id() == current.id()
                }
            })
            .flat_map(|candidate| candidate.mount_points().iter().cloned())
            .collect())
    }

    fn perform_validated_with_request(
        &self,
        volume: &DeviceDescriptor,
        expected_owner: Option<&str>,
        action: VolumeAction,
        unlock_secret: Option<&str>,
        request: &UDisksRequest,
    ) -> Result<(), UDisksError> {
        let _ = self.validate_action(volume, expected_owner, action, request)?;
        let owner = expected_owner.ok_or(UDisksError::StaleObject)?.to_owned();
        block_on_request(request, async {
            if self.service_owner().await? != owner {
                return Err(UDisksError::StaleObject);
            }
            let options: HashMap<&str, Value<'_>> = HashMap::new();
            match action {
                VolumeAction::Mount => {
                    let _: String = self
                        .proxy_at(&owner, volume.object_path(), FILESYSTEM)
                        .await?
                        .call("Mount", &(options,))
                        .await
                        .map_err(map_zbus_error)?;
                }
                VolumeAction::Unmount => {
                    let _: () = self
                        .proxy_at(&owner, volume.object_path(), FILESYSTEM)
                        .await?
                        .call("Unmount", &(options,))
                        .await
                        .map_err(map_zbus_error)?;
                }
                VolumeAction::Eject | VolumeAction::PowerOff => {
                    let drive = volume.drive_path().ok_or(UDisksError::ActionUnavailable)?;
                    let method = if action == VolumeAction::Eject {
                        "Eject"
                    } else {
                        "PowerOff"
                    };
                    let _: () = self
                        .proxy_at(&owner, drive, DRIVE)
                        .await?
                        .call(method, &(options,))
                        .await
                        .map_err(map_zbus_error)?;
                }
                VolumeAction::Unlock => {
                    let secret = unlock_secret.ok_or(UDisksError::UnlockSecretRequired)?;
                    let _: OwnedObjectPath = self
                        .proxy_at(&owner, volume.object_path(), ENCRYPTED)
                        .await?
                        .call("Unlock", &(secret, options))
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

#[derive(Clone, Copy, Debug)]
enum BlockIdentity<'a> {
    WholeDevice,
    Link(&'a std::path::Path),
    Partition(&'a str),
    Ambiguous,
}

#[derive(Debug, Eq, PartialEq)]
struct DeviceIdentity {
    value: String,
    stable: bool,
    hardware_identity: Option<Box<str>>,
}

#[derive(Debug)]
struct ObservedDevice {
    descriptor: DeviceDescriptor,
    fallback_id: VolumeId,
    hardware_identity: Option<Box<str>>,
    drive_object: Option<Box<str>>,
}

fn stable_device_id(
    uuid: &str,
    drive: &str,
    block: BlockIdentity<'_>,
    device: &std::path::Path,
    object: &str,
) -> DeviceIdentity {
    let stable = match block {
        BlockIdentity::Link(_) => true,
        BlockIdentity::WholeDevice | BlockIdentity::Partition(_) => !drive.is_empty(),
        BlockIdentity::Ambiguous => false,
    };
    let value = if stable {
        encode_device_id("device-", stable_identity_bytes(drive, block))
    } else {
        ambiguous_device_id(uuid, device, object)
            .as_str()
            .to_owned()
    };
    DeviceIdentity {
        value,
        stable,
        hardware_identity: if stable
            && matches!(
                block,
                BlockIdentity::WholeDevice | BlockIdentity::Partition(_)
            ) {
            Some(drive.into())
        } else {
            None
        },
    }
}

fn stable_identity_bytes(drive: &str, block: BlockIdentity<'_>) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;

    match block {
        BlockIdentity::WholeDevice => drive.as_bytes().to_vec(),
        BlockIdentity::Link(path) => path.as_os_str().as_bytes().to_vec(),
        BlockIdentity::Partition(partition) => {
            let mut bytes = drive.as_bytes().to_vec();
            bytes.push(0);
            bytes.extend_from_slice(partition.as_bytes());
            bytes
        }
        BlockIdentity::Ambiguous => unreachable!("ambiguous identities are encoded separately"),
    }
}

fn ambiguous_device_id(uuid: &str, device: &std::path::Path, object: &str) -> VolumeId {
    use std::os::unix::ffi::OsStrExt as _;

    // There is no stable per-block discriminator. Keep the object visible and
    // collision-free for this session, but disable actions because it cannot be
    // reconciled safely after object or kernel churn.
    let mut bytes = uuid.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(device.as_os_str().as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(object.as_bytes());
    VolumeId::new(encode_device_id("device-ambiguous-", bytes))
        .expect("hex-encoded device identities are always valid")
}

fn encode_device_id(prefix: &str, bytes: Vec<u8>) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2 + prefix.len());
    encoded.push_str(prefix);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn reconcile_device_identities(devices: Vec<ObservedDevice>) -> Vec<DeviceDescriptor> {
    let mut id_counts = BTreeMap::<VolumeId, usize>::new();
    let mut hardware_drives = BTreeMap::<Box<str>, BTreeSet<Box<str>>>::new();
    for device in &devices {
        *id_counts.entry(device.descriptor.id().clone()).or_default() += 1;
        if let Some(identity) = &device.hardware_identity {
            let drives = hardware_drives.entry(identity.clone()).or_default();
            if let Some(drive) = &device.drive_object {
                drives.insert(drive.clone());
            }
        }
    }

    devices
        .into_iter()
        .map(|device| {
            let duplicate_id = id_counts
                .get(device.descriptor.id())
                .is_some_and(|count| *count > 1);
            let duplicated_hardware = device.hardware_identity.as_ref().is_some_and(|identity| {
                hardware_drives
                    .get(identity)
                    .is_none_or(|drives| drives.len() != 1)
            });
            if duplicate_id || duplicated_hardware {
                device
                    .descriptor
                    .with_ambiguous_identity(device.fallback_id)
            } else {
                device.descriptor
            }
        })
        .collect()
}

fn is_stable_block_link(path: &std::path::Path) -> bool {
    path.starts_with("/dev/disk/by-id") || path.starts_with("/dev/disk/by-partuuid")
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
                    let _ = sender.try_send(super::VolumeTrigger::ServiceOwnerChanged);
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
    let _ = sender.try_send(super::VolumeTrigger::UDisksChanged);
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
            BlockIdentity::Link(std::path::Path::new("/dev/disk/by-id/a")),
            std::path::Path::new("/dev/disk/by-id/a"),
            "/org/freedesktop/UDisks2/block_devices/sdb1",
        );
        let second = stable_device_id(
            "same-uuid",
            "SERIAL-B",
            BlockIdentity::Link(std::path::Path::new("/dev/disk/by-id/b")),
            std::path::Path::new("/dev/disk/by-id/b"),
            "/org/freedesktop/UDisks2/block_devices/sdc1",
        );
        assert_ne!(first, second);
        assert_eq!(
            first,
            stable_device_id(
                "same-uuid",
                "SERIAL-A",
                BlockIdentity::Link(std::path::Path::new("/dev/disk/by-id/a")),
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
            BlockIdentity::WholeDevice,
            std::path::Path::new("/dev/sdb1"),
            "/org/freedesktop/UDisks2/block_devices/sdb1",
        );
        let after = stable_device_id(
            "same-uuid",
            "wwn-123",
            BlockIdentity::WholeDevice,
            std::path::Path::new("/dev/sdz1"),
            "/org/freedesktop/UDisks2/block_devices/sdz1",
        );
        assert_eq!(before, after);
    }

    #[test]
    fn same_drive_block_identities_are_distinct_and_stable_across_path_churn() {
        let first_before = stable_device_id(
            "cloned-uuid",
            "wwn-123",
            BlockIdentity::Link(std::path::Path::new("/dev/disk/by-id/wwn-123-part1")),
            std::path::Path::new("/dev/disk/by-id/wwn-123-part1"),
            "/org/freedesktop/UDisks2/block_devices/sdb1",
        );
        let first_after = stable_device_id(
            "cloned-uuid",
            "wwn-123",
            BlockIdentity::Link(std::path::Path::new("/dev/disk/by-id/wwn-123-part1")),
            std::path::Path::new("/dev/disk/by-id/wwn-123-part1"),
            "/org/freedesktop/UDisks2/block_devices/sdz1",
        );
        let second = stable_device_id(
            "cloned-uuid",
            "wwn-123",
            BlockIdentity::Link(std::path::Path::new("/dev/disk/by-id/wwn-123-part2")),
            std::path::Path::new("/dev/disk/by-id/wwn-123-part2"),
            "/org/freedesktop/UDisks2/block_devices/sdb2",
        );
        let blank = stable_device_id(
            "",
            "wwn-123",
            BlockIdentity::Link(std::path::Path::new("/dev/disk/by-id/wwn-123-part3")),
            std::path::Path::new("/dev/disk/by-id/wwn-123-part3"),
            "/org/freedesktop/UDisks2/block_devices/sdb3",
        );

        assert_eq!(first_before, first_after);
        assert_ne!(first_before, second);
        assert_ne!(second, blank);
        assert_stable_link_ignores_hardware_metadata();

        let mut model = crate::volumes::VolumeModel::default();
        let volumes = [first_before, second, blank].map(|value| {
            assert!(value.stable);
            let id = VolumeId::new(value.value).unwrap();
            let descriptor = DeviceDescriptor::new(id, "/org/freedesktop/UDisks2/block");
            crate::volumes::Volume::from_device(descriptor, crate::volumes::ServiceState::Available)
        });
        assert!(model.replace(Some(":1.42".into()), volumes).is_ok());
        assert_eq!(model.volumes().len(), 3);
    }

    fn assert_stable_link_ignores_hardware_metadata() {
        let trusted_link = std::path::Path::new("/dev/disk/by-id/usb-trusted-part1");
        let with_serial = stable_device_id(
            "same-uuid",
            "sometimes-present",
            BlockIdentity::Link(trusted_link),
            std::path::Path::new("/dev/sdb1"),
            "/org/freedesktop/UDisks2/block_devices/sdb1",
        );
        let without_serial = stable_device_id(
            "same-uuid",
            "",
            BlockIdentity::Link(trusted_link),
            std::path::Path::new("/dev/sdz1"),
            "/org/freedesktop/UDisks2/block_devices/sdz1",
        );
        assert!(with_serial.stable && without_serial.stable);
        assert_eq!(with_serial, without_serial);
    }

    #[test]
    fn ambiguous_same_drive_blocks_remain_visible_but_are_not_stable() {
        let first = stable_device_id(
            "",
            "wwn-123",
            BlockIdentity::Ambiguous,
            std::path::Path::new("/dev/sdb1"),
            "/org/freedesktop/UDisks2/block_devices/sdb1",
        );
        let second = stable_device_id(
            "",
            "wwn-123",
            BlockIdentity::Ambiguous,
            std::path::Path::new("/dev/sdb2"),
            "/org/freedesktop/UDisks2/block_devices/sdb2",
        );
        assert!(!first.stable && !second.stable);
        assert_ne!(first.value, second.value);
        assert!(first.value.starts_with("device-ambiguous-"));
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
