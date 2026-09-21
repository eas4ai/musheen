//! Linux volume discovery and UDisks2 operations.
//!
//! This module is the single application boundary for mount-table parsing,
//! capacity probing, UDisks2 D-Bus calls, and operation-use checks.

mod model;
mod mounts;
mod runtime;
mod udisks;

pub use model::*;
pub use mounts::*;
pub use runtime::*;
pub use udisks::*;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

pub struct VolumeSubscription {
    receiver: async_channel::Receiver<VolumeTrigger>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl fmt::Debug for VolumeSubscription {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VolumeSubscription")
            .field("closed", &self.receiver.is_closed())
            .finish_non_exhaustive()
    }
}

impl VolumeSubscription {
    #[must_use]
    pub fn system() -> Self {
        let (sender, receiver) = async_channel::bounded(32);
        // UDisks2 is the primary device-event source. The procfs listener
        // independently covers kernel mounts that UDisks2 does not own.
        let mut threads = spawn_udisks_event_listener(sender.clone());
        if let Some(thread) = spawn_mount_table_listener(sender.clone()) {
            threads.push(thread);
        }
        let _ = sender.try_send(VolumeTrigger::MountTableChanged);
        Self { receiver, threads }
    }

    pub async fn recv(&self) -> Result<VolumeTrigger, async_channel::RecvError> {
        self.receiver.recv().await
    }

    pub(crate) fn recv_timeout(&self, timeout: std::time::Duration) -> Option<VolumeTrigger> {
        futures_lite::future::block_on(futures_lite::future::race(
            async { self.receiver.recv().await.ok() },
            async {
                futures_lite::future::yield_now().await;
                std::thread::sleep(timeout);
                None
            },
        ))
    }
}

impl Drop for VolumeSubscription {
    fn drop(&mut self) {
        self.receiver.close();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn spawn_mount_table_listener(
    sender: async_channel::Sender<VolumeTrigger>,
) -> Option<std::thread::JoinHandle<()>> {
    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
    use std::fs::File;
    use std::io::{Read as _, Seek as _};
    use std::os::fd::AsFd as _;
    use std::thread;

    thread::Builder::new()
        .name("musheen-mount-events".into())
        .spawn(move || {
            let Ok(mut mountinfo) = File::open("/proc/self/mountinfo") else {
                return;
            };
            let mut contents = Vec::new();
            let _ = mountinfo.read_to_end(&mut contents);
            while !sender.is_closed() {
                // `poll(2)` blocks on procfs's mount-namespace change edge.
                // The timeout only observes receiver shutdown; a timeout never
                // emits a trigger or rereads the mount table.
                let changed = {
                    let mut descriptors = [PollFd::new(
                        mountinfo.as_fd(),
                        PollFlags::POLLERR | PollFlags::POLLPRI,
                    )];
                    match poll(&mut descriptors, PollTimeout::from(1_000_u16)) {
                        Ok(ready) if ready > 0 => descriptors[0]
                            .revents()
                            .is_some_and(|events| !events.is_empty()),
                        Ok(_) => false,
                        Err(_) => return,
                    }
                };
                if !changed {
                    continue;
                }
                // Consuming the new snapshot acknowledges procfs's edge before
                // waiting for the next mount-namespace change.
                contents.clear();
                let _ = mountinfo.rewind();
                let _ = mountinfo.read_to_end(&mut contents);
                if sender.try_send(VolumeTrigger::MountTableChanged).is_err() && sender.is_closed()
                {
                    return;
                }
            }
        })
        .ok()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VolumeAction {
    Mount,
    Unmount,
    Eject,
    Unlock,
    PowerOff,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MountOperation(u64);

impl MountOperation {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationUse {
    id: MountOperation,
    label: Box<str>,
}

impl OperationUse {
    #[must_use]
    pub fn new(id: MountOperation, label: impl Into<Box<str>>) -> Self {
        Self {
            id,
            label: label.into(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> MountOperation {
        self.id
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

pub trait OperationUsage: Send + Sync {
    fn operations_using(&self, mounts: &[PathBuf]) -> Vec<OperationUse>;
    fn cancel(&self, operations: &[OperationUse]) -> Result<(), VolumeError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoOperationUsage;

impl OperationUsage for NoOperationUsage {
    fn operations_using(&self, _mounts: &[PathBuf]) -> Vec<OperationUse> {
        Vec::new()
    }

    fn cancel(&self, _operations: &[OperationUse]) -> Result<(), VolumeError> {
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageResolution {
    Refuse,
    /// Cancel only the operations the user reviewed and approved.
    CancelApproved(Vec<OperationUse>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VolumeTrigger {
    UDisksChanged,
    MountTableChanged,
    ServiceOwnerChanged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VolumeError {
    MountTable(Box<str>),
    Capacity { path: PathBuf, reason: Box<str> },
    ServiceUnavailable(Box<str>),
    Timeout(Box<str>),
    Disconnected(Box<str>),
    AuthorizationRequired(Box<str>),
    Busy(Box<str>),
    Unsupported(Box<str>),
    Disappeared(VolumeId),
    InUse(Vec<OperationUse>),
    CancellationFailed(Box<str>),
    Protocol(Box<str>),
}

impl fmt::Display for VolumeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MountTable(reason) => {
                write!(formatter, "the mount table is unavailable: {reason}")
            }
            Self::Capacity { path, reason } => {
                write!(
                    formatter,
                    "could not read capacity for {}: {reason}",
                    path.display()
                )
            }
            Self::ServiceUnavailable(reason) => {
                write!(formatter, "UDisks2 is unavailable: {reason}")
            }
            Self::Timeout(reason) => write!(formatter, "UDisks2 did not respond: {reason}"),
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
            Self::Disappeared(id) => write!(formatter, "volume {id} disappeared"),
            Self::InUse(operations) => write!(
                formatter,
                "the volume is used by {} application operation(s)",
                operations.len()
            ),
            Self::CancellationFailed(reason) => {
                write!(formatter, "could not cancel volume users: {reason}")
            }
            Self::Protocol(reason) => {
                write!(formatter, "invalid desktop-service response: {reason}")
            }
        }
    }
}

impl std::error::Error for VolumeError {}

impl From<UDisksError> for VolumeError {
    fn from(error: UDisksError) -> Self {
        match error {
            UDisksError::Unavailable(reason) => Self::ServiceUnavailable(reason),
            UDisksError::Timeout(reason) => Self::Timeout(reason),
            UDisksError::Disconnected(reason) => Self::Disconnected(reason),
            UDisksError::AuthorizationRequired(reason) => Self::AuthorizationRequired(reason),
            UDisksError::Busy(reason) => Self::Busy(reason),
            UDisksError::Unsupported(reason) => Self::Unsupported(reason),
            UDisksError::StaleObject => Self::Protocol("the UDisks2 object is stale".into()),
            UDisksError::Protocol(reason) => Self::Protocol(reason),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefreshReport {
    events: Vec<VolumeEvent>,
    service_state: ServiceState,
    warning: Option<VolumeError>,
}

impl RefreshReport {
    #[must_use]
    pub fn events(&self) -> &[VolumeEvent] {
        &self.events
    }

    #[must_use]
    pub const fn service_state(&self) -> ServiceState {
        self.service_state
    }

    #[must_use]
    pub const fn warning(&self) -> Option<&VolumeError> {
        self.warning.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationOutcome {
    volume_present: bool,
    refresh: RefreshReport,
}

impl OperationOutcome {
    #[must_use]
    pub const fn volume_present(&self) -> bool {
        self.volume_present
    }

    #[must_use]
    pub const fn refresh(&self) -> &RefreshReport {
        &self.refresh
    }
}

pub struct VolumeService {
    backend: Arc<dyn UDisksBackend>,
    mounts: Arc<dyn MountProvider>,
    usage: Arc<dyn OperationUsage>,
    model: VolumeModel,
}

impl fmt::Debug for VolumeService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VolumeService")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl VolumeService {
    #[must_use]
    pub fn new(
        backend: Arc<dyn UDisksBackend>,
        mounts: Arc<dyn MountProvider>,
        usage: Arc<dyn OperationUsage>,
    ) -> Self {
        Self {
            backend,
            mounts,
            usage,
            model: VolumeModel::default(),
        }
    }

    pub fn system() -> Result<Self, VolumeError> {
        Self::system_with_usage(Arc::new(NoOperationUsage))
    }

    pub fn system_with_usage(usage: Arc<dyn OperationUsage>) -> Result<Self, VolumeError> {
        Ok(Self::new(
            Arc::new(ReconnectingUDisksBackend),
            Arc::new(ProcMountProvider::system()),
            usage,
        ))
    }

    #[must_use]
    pub const fn model(&self) -> &VolumeModel {
        &self.model
    }

    pub fn handle(&mut self, _trigger: VolumeTrigger) -> Result<RefreshReport, VolumeError> {
        self.refresh()
    }

    pub fn refresh(&mut self) -> Result<RefreshReport, VolumeError> {
        let mounts = deduplicate_mounts(self.mounts.snapshot()?);
        let (snapshot, service_state, warning) = match self.backend.snapshot() {
            Ok(snapshot) => (Some(snapshot), ServiceState::Available, None),
            Err(error) => {
                let state = match error {
                    UDisksError::Unavailable(_) => ServiceState::Unavailable,
                    UDisksError::Timeout(_) => ServiceState::Slow,
                    UDisksError::Disconnected(_) => ServiceState::Disconnected,
                    _ => ServiceState::Unavailable,
                };
                (None, state, Some(VolumeError::from(error)))
            }
        };

        let owner = snapshot.as_ref().map(|snapshot| snapshot.owner().into());
        let devices = snapshot
            .as_ref()
            .map_or(&[][..], |snapshot| snapshot.devices());
        let mut volumes = devices
            .iter()
            .cloned()
            .map(|device| Volume::from_device(device, service_state))
            .collect::<Vec<_>>();
        let mut consumed = BTreeSet::new();
        for volume in &mut volumes {
            let Some(descriptor) = volume.descriptor().cloned() else {
                continue;
            };
            for (destination, record) in &mounts {
                if descriptor.device() == record.source()
                    || descriptor
                        .mount_points()
                        .contains(&record.destination().to_path_buf())
                {
                    let capacity = self.mounts.capacity(record.destination()).ok();
                    volume.attach_mount(record, capacity);
                    consumed.insert(destination.clone());
                }
            }
        }
        let mut mount_only = BTreeMap::<PathBuf, Volume>::new();
        for (destination, record) in &mounts {
            if !consumed.contains(destination) {
                let capacity = self.mounts.capacity(record.destination()).ok();
                mount_only
                    .entry(record.source().to_path_buf())
                    .and_modify(|volume| volume.attach_mount(record, capacity))
                    .or_insert_with(|| Volume::from_mount(record, capacity, service_state));
            }
        }
        volumes.extend(mount_only.into_values());
        for volume in &mut volumes {
            volume.reconcile_mount_capabilities();
            volume.mark_service_state(service_state);
        }
        let events = self
            .model
            .replace(owner, volumes)
            .map_err(|error| VolumeError::Protocol(error.to_string().into()))?;
        Ok(RefreshReport {
            events,
            service_state,
            warning,
        })
    }

    pub fn perform(
        &mut self,
        id: &VolumeId,
        action: VolumeAction,
        usage_resolution: UsageResolution,
        unlock_secret: Option<&str>,
    ) -> Result<OperationOutcome, VolumeError> {
        let volume = self
            .model
            .get(id)
            .cloned()
            .ok_or_else(|| VolumeError::Disappeared(id.clone()))?;
        let descriptor = volume
            .descriptor()
            .cloned()
            .ok_or_else(|| VolumeError::Unsupported("UDisks2 does not expose this mount".into()))?;
        ensure_supported(&volume, action, unlock_secret)?;

        self.resolve_usage(&volume, action, usage_resolution)?;

        if let Err(error) = self.backend.perform(&descriptor, action, unlock_secret) {
            if error == UDisksError::StaleObject {
                let _ = self.refresh()?;
                if self.model.get(id).is_none() {
                    return Err(VolumeError::Disappeared(id.clone()));
                }
            }
            return Err(error.into());
        }
        let refresh = self.refresh()?;
        Ok(OperationOutcome {
            volume_present: self.model.get(id).is_some(),
            refresh,
        })
    }

    fn resolve_usage(
        &self,
        volume: &Volume,
        action: VolumeAction,
        resolution: UsageResolution,
    ) -> Result<(), VolumeError> {
        if !matches!(
            action,
            VolumeAction::Unmount | VolumeAction::Eject | VolumeAction::PowerOff
        ) {
            return Ok(());
        }
        let operations = self.usage.operations_using(volume.mount_points());
        if operations.is_empty() {
            return Ok(());
        }
        let UsageResolution::CancelApproved(approved) = resolution else {
            return Err(VolumeError::InUse(operations));
        };
        let ids =
            |uses: &[OperationUse]| uses.iter().map(OperationUse::id).collect::<BTreeSet<_>>();
        if ids(&operations) != ids(&approved) {
            return Err(VolumeError::InUse(operations));
        }
        self.usage.cancel(&approved)
    }
}

#[derive(Clone, Copy, Debug)]
struct ReconnectingUDisksBackend;

impl UDisksBackend for ReconnectingUDisksBackend {
    fn snapshot(&self) -> Result<BackendSnapshot, UDisksError> {
        ZbusUDisksBackend::connect_system()?.snapshot()
    }

    fn perform(
        &self,
        volume: &DeviceDescriptor,
        action: VolumeAction,
        unlock_secret: Option<&str>,
    ) -> Result<(), UDisksError> {
        ZbusUDisksBackend::connect_system()?.perform(volume, action, unlock_secret)
    }
}

fn deduplicate_mounts(records: Vec<MountRecord>) -> BTreeMap<PathBuf, MountRecord> {
    let mut unique = BTreeMap::new();
    for record in records {
        unique
            .entry(record.destination().to_path_buf())
            .or_insert(record);
    }
    unique
}

fn ensure_supported(
    volume: &Volume,
    action: VolumeAction,
    unlock_secret: Option<&str>,
) -> Result<(), VolumeError> {
    let capabilities = volume.capabilities();
    let supported = match action {
        VolumeAction::Mount => capabilities.can_mount,
        VolumeAction::Unmount => capabilities.can_unmount,
        VolumeAction::Eject => capabilities.can_eject,
        VolumeAction::Unlock => capabilities.can_unlock,
        VolumeAction::PowerOff => capabilities.can_power_off,
    };
    if !supported {
        return Err(VolumeError::Unsupported(
            "UDisks2 did not advertise this action".into(),
        ));
    }
    if action == VolumeAction::Unlock && unlock_secret.is_none() {
        return Err(VolumeError::AuthorizationRequired(
            "an unlock secret is required".into(),
        ));
    }
    Ok(())
}
