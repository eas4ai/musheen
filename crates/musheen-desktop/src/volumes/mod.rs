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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const VOLUME_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
static SYSTEM_CAPACITY_PROBE: OnceLock<Arc<AtomicBool>> = OnceLock::new();

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
    #[doc(hidden)]
    #[must_use]
    pub fn from_parts(
        receiver: async_channel::Receiver<VolumeTrigger>,
        threads: Vec<std::thread::JoinHandle<()>>,
    ) -> Self {
        Self { receiver, threads }
    }

    #[must_use]
    pub fn system() -> Self {
        Self::with_udisks(UDisksBusConfig::system(), true)
    }

    #[must_use]
    pub fn with_udisks(config: UDisksBusConfig, include_mount_table: bool) -> Self {
        let (sender, receiver) = async_channel::bounded(32);
        // UDisks2 is the primary device-event source. The procfs listener
        // independently covers kernel mounts that UDisks2 does not own.
        let mut threads = spawn_udisks_event_listener_with_config(sender.clone(), config);
        if include_mount_table && let Some(thread) = spawn_mount_table_listener(sender.clone()) {
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

    fn reserve<'a>(
        &'a self,
        _mounts: &[PathBuf],
    ) -> Result<Box<dyn OperationReservation + 'a>, VolumeError> {
        Ok(Box::new(NoOperationReservation))
    }
}

pub trait OperationReservation {
    fn operations_using(&self) -> Vec<OperationUse>;
}

struct NoOperationReservation;

impl OperationReservation for NoOperationReservation {
    fn operations_using(&self) -> Vec<OperationUse> {
        Vec::new()
    }
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
    DeadlineExceeded,
    WorkerStopped,
    UnlockSecretRequired,
    ActionUnavailable,
    MountNotExposed,
    StaleObject,
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
            Self::DeadlineExceeded => formatter.write_str("operation deadline exceeded"),
            Self::WorkerStopped => formatter.write_str("the volume worker stopped"),
            Self::UnlockSecretRequired => formatter.write_str("an unlock secret is required"),
            Self::ActionUnavailable => formatter.write_str("UDisks2 did not advertise this action"),
            Self::MountNotExposed => formatter.write_str("UDisks2 does not expose this mount"),
            Self::StaleObject => formatter.write_str("the UDisks2 object is stale"),
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
            UDisksError::DeadlineExceeded => Self::DeadlineExceeded,
            UDisksError::WorkerStopped => Self::WorkerStopped,
            UDisksError::UnlockSecretRequired => Self::UnlockSecretRequired,
            UDisksError::ActionUnavailable => Self::ActionUnavailable,
            UDisksError::StaleObject => Self::StaleObject,
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
    capacity_probe_active: Arc<AtomicBool>,
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
            capacity_probe_active: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn system() -> Result<Self, VolumeError> {
        Self::system_with_usage(Arc::new(NoOperationUsage))
    }

    pub fn system_with_usage(usage: Arc<dyn OperationUsage>) -> Result<Self, VolumeError> {
        let mut service = Self::new(
            Arc::new(ReconnectingUDisksBackend::new(UDisksBusConfig::system())),
            Arc::new(ProcMountProvider::system()),
            usage,
        );
        service.capacity_probe_active =
            Arc::clone(SYSTEM_CAPACITY_PROBE.get_or_init(|| Arc::new(AtomicBool::new(false))));
        Ok(service)
    }

    #[must_use]
    pub const fn model(&self) -> &VolumeModel {
        &self.model
    }

    pub fn handle(&mut self, _trigger: VolumeTrigger) -> Result<RefreshReport, VolumeError> {
        self.refresh()
    }

    pub fn refresh(&mut self) -> Result<RefreshReport, VolumeError> {
        self.refresh_with_request(&UDisksRequest::with_timeout(VOLUME_REQUEST_TIMEOUT))
    }

    pub(crate) fn handle_with_request(
        &mut self,
        _trigger: VolumeTrigger,
        request: &UDisksRequest,
    ) -> Result<RefreshReport, VolumeError> {
        self.refresh_with_request(request)
    }

    fn refresh_with_request(
        &mut self,
        request: &UDisksRequest,
    ) -> Result<RefreshReport, VolumeError> {
        request.check()?;
        let mounts = deduplicate_mounts(self.mounts.snapshot()?);
        let (snapshot, service_state, warning) = match self.backend.snapshot_with_request(request) {
            Ok(snapshot) => (Some(snapshot), ServiceState::Available, None),
            Err(error) => {
                let state = match error {
                    UDisksError::Unavailable(_) => ServiceState::Unavailable,
                    UDisksError::Timeout(_) => ServiceState::Slow,
                    UDisksError::DeadlineExceeded => ServiceState::Slow,
                    UDisksError::Disconnected(_) => ServiceState::Disconnected,
                    UDisksError::WorkerStopped => ServiceState::Disconnected,
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
        if snapshot.is_none() {
            // A service restart must not turn a known UDisks device into a
            // different mount-only volume. Retain its stable descriptor while
            // disabling capabilities until the owner returns.
            volumes.extend(
                self.model
                    .volumes()
                    .into_iter()
                    .filter_map(|volume| volume.descriptor().cloned())
                    .map(|descriptor| Volume::from_device(descriptor, service_state)),
            );
        }
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
                    let capacity = self.probe_capacity(record.destination(), request);
                    volume.attach_mount(record, capacity);
                    consumed.insert(destination.clone());
                }
            }
        }
        let prior_mount_only = self
            .model
            .volumes()
            .into_iter()
            .filter(|volume| volume.descriptor().is_none())
            .cloned()
            .collect::<Vec<_>>();
        let reconciled_mount_ids =
            reconcile_anonymous_mount_ids(&mounts, &consumed, &prior_mount_only);
        let mut mount_only = BTreeMap::<VolumeId, Volume>::new();
        for (destination, record) in &mounts {
            if !consumed.contains(destination) {
                let capacity = self.probe_capacity(record.destination(), request);
                if record.source().starts_with("/dev/") {
                    let id = VolumeId::from_mount(record);
                    mount_only
                        .entry(id)
                        .and_modify(|volume| volume.attach_mount(record, capacity))
                        .or_insert_with(|| Volume::from_mount(record, capacity, service_state));
                } else {
                    // A destination is the only stable identity signal exposed
                    // by procfs for anonymous mounts. Preserve an unchanged
                    // destination (including a renamed source), but never let a
                    // new mount steal a removed mount's ID by iteration order.
                    let mut reconciled = reconciled_mount_ids
                        .get(destination)
                        .cloned()
                        .unwrap_or_else(|| VolumeId::from_mount(record));
                    let mut occurrence = 1;
                    while mount_only.contains_key(&reconciled) {
                        reconciled = VolumeId::from_mount_occurrence(record, occurrence);
                        occurrence += 1;
                    }
                    mount_only.insert(
                        reconciled.clone(),
                        Volume::from_mount_with_id(reconciled, record, capacity, service_state),
                    );
                }
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
        self.perform_with_request(
            id,
            action,
            usage_resolution,
            unlock_secret,
            &UDisksRequest::with_timeout(VOLUME_REQUEST_TIMEOUT),
        )
    }

    pub(crate) fn perform_with_request(
        &mut self,
        id: &VolumeId,
        action: VolumeAction,
        usage_resolution: UsageResolution,
        unlock_secret: Option<&str>,
        request: &UDisksRequest,
    ) -> Result<OperationOutcome, VolumeError> {
        request.check()?;
        let volume = self
            .model
            .get(id)
            .cloned()
            .ok_or_else(|| VolumeError::Disappeared(id.clone()))?;
        let descriptor = volume
            .descriptor()
            .cloned()
            .ok_or(VolumeError::MountNotExposed)?;
        ensure_supported(&volume, action, unlock_secret)?;

        if matches!(
            action,
            VolumeAction::Unmount | VolumeAction::Eject | VolumeAction::PowerOff
        ) {
            let validated_mounts = self.backend.validate_action(
                &descriptor,
                self.model.service_owner(),
                action,
                request,
            )?;
            request.check()?;
            let live_mounts = self.mounts.snapshot()?;
            request.check()?;
            let affected_devices = self.model.devices_affected_by(id, action);
            let usage_mounts =
                reconcile_mount_aliases(&validated_mounts, &affected_devices, &live_mounts);
            let canceled = self.resolve_usage(&usage_mounts, action, usage_resolution, request)?;
            let reservation_mounts = if canceled {
                request.check()?;
                let live_mounts = self.mounts.snapshot()?;
                request.check()?;
                reconcile_mount_aliases(&validated_mounts, &affected_devices, &live_mounts)
            } else {
                usage_mounts
            };
            let reservation = self.usage.reserve(&reservation_mounts)?;
            let newly_active = reservation.operations_using();
            if !newly_active.is_empty() {
                return Err(VolumeError::InUse(newly_active));
            }
            if let Err(error) = self.backend.perform_validated_with_request(
                &descriptor,
                self.model.service_owner(),
                action,
                unlock_secret,
                request,
            ) {
                drop(reservation);
                return self.handle_action_error(id, error, request);
            }
            drop(reservation);
        } else if let Err(error) = self.backend.perform_validated_with_request(
            &descriptor,
            self.model.service_owner(),
            action,
            unlock_secret,
            request,
        ) {
            return self.handle_action_error(id, error, request);
        }
        let refresh = self.refresh_with_request(request)?;
        Ok(OperationOutcome {
            volume_present: self.model.get(id).is_some(),
            refresh,
        })
    }

    fn handle_action_error(
        &mut self,
        id: &VolumeId,
        error: UDisksError,
        request: &UDisksRequest,
    ) -> Result<OperationOutcome, VolumeError> {
        if error == UDisksError::StaleObject {
            let _ = self.refresh_with_request(request)?;
            if self.model.get(id).is_none() {
                return Err(VolumeError::Disappeared(id.clone()));
            }
        }
        Err(error.into())
    }

    fn resolve_usage(
        &self,
        mounts: &[PathBuf],
        action: VolumeAction,
        resolution: UsageResolution,
        request: &UDisksRequest,
    ) -> Result<bool, VolumeError> {
        if !matches!(
            action,
            VolumeAction::Unmount | VolumeAction::Eject | VolumeAction::PowerOff
        ) {
            return Ok(false);
        }
        let operations = self.usage.operations_using(mounts);
        if operations.is_empty() {
            return Ok(false);
        }
        let UsageResolution::CancelApproved(approved) = resolution else {
            return Err(VolumeError::InUse(operations));
        };
        let ids =
            |uses: &[OperationUse]| uses.iter().map(OperationUse::id).collect::<BTreeSet<_>>();
        if ids(&operations) != ids(&approved) {
            return Err(VolumeError::InUse(operations));
        }
        self.usage.cancel(&approved)?;
        loop {
            request.check()?;
            let active = self.usage.operations_using(mounts);
            if active.is_empty() {
                return Ok(true);
            }
            if !ids(&active).is_subset(&ids(&approved)) {
                return Err(VolumeError::InUse(active));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn probe_capacity(&self, path: &std::path::Path, request: &UDisksRequest) -> Option<Capacity> {
        probe_capacity(
            Arc::clone(&self.mounts),
            path,
            request,
            Arc::clone(&self.capacity_probe_active),
        )
    }
}

fn probe_capacity(
    provider: Arc<dyn MountProvider>,
    path: &std::path::Path,
    request: &UDisksRequest,
    active: Arc<AtomicBool>,
) -> Option<Capacity> {
    if active.swap(true, Ordering::AcqRel) {
        return None;
    }
    let path = path.to_path_buf();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let probe_active = Arc::clone(&active);
    if std::thread::Builder::new()
        .name("musheen-capacity-probe".into())
        .spawn(move || {
            let _lease = CapacityProbeLease(probe_active);
            let result = provider.capacity(&path).ok();
            let _ = sender.send(result);
        })
        .is_err()
    {
        active.store(false, Ordering::Release);
        return None;
    }
    loop {
        if request.check().is_err() {
            return None;
        }
        match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(result) => return result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
}

struct CapacityProbeLease(Arc<AtomicBool>);

impl Drop for CapacityProbeLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct ReconnectingUDisksBackend {
    config: UDisksBusConfig,
}

impl ReconnectingUDisksBackend {
    #[must_use]
    pub const fn new(config: UDisksBusConfig) -> Self {
        Self { config }
    }

    fn connect(&self, request: &UDisksRequest) -> Result<ZbusUDisksBackend, UDisksError> {
        ZbusUDisksBackend::connect(self.config.clone(), request.remaining())
    }
}

impl UDisksBackend for ReconnectingUDisksBackend {
    fn snapshot(&self) -> Result<BackendSnapshot, UDisksError> {
        ZbusUDisksBackend::connect(self.config.clone(), VOLUME_REQUEST_TIMEOUT)?.snapshot()
    }

    fn perform(
        &self,
        volume: &DeviceDescriptor,
        action: VolumeAction,
        unlock_secret: Option<&str>,
    ) -> Result<(), UDisksError> {
        ZbusUDisksBackend::connect(self.config.clone(), VOLUME_REQUEST_TIMEOUT)?.perform(
            volume,
            action,
            unlock_secret,
        )
    }

    fn snapshot_with_request(
        &self,
        request: &UDisksRequest,
    ) -> Result<BackendSnapshot, UDisksError> {
        request.check()?;
        let backend = self.connect(request)?;
        backend.snapshot_with_request(request)
    }

    fn perform_with_request(
        &self,
        volume: &DeviceDescriptor,
        action: VolumeAction,
        unlock_secret: Option<&str>,
        request: &UDisksRequest,
    ) -> Result<(), UDisksError> {
        request.check()?;
        let backend = self.connect(request)?;
        backend.perform_with_request(volume, action, unlock_secret, request)
    }

    fn perform_validated_with_request(
        &self,
        volume: &DeviceDescriptor,
        expected_owner: Option<&str>,
        action: VolumeAction,
        unlock_secret: Option<&str>,
        request: &UDisksRequest,
    ) -> Result<(), UDisksError> {
        request.check()?;
        let backend = self.connect(request)?;
        backend.perform_validated_with_request(
            volume,
            expected_owner,
            action,
            unlock_secret,
            request,
        )
    }

    fn validate_action(
        &self,
        volume: &DeviceDescriptor,
        expected_owner: Option<&str>,
        action: VolumeAction,
        request: &UDisksRequest,
    ) -> Result<Vec<PathBuf>, UDisksError> {
        self.connect(request)?
            .validate_action(volume, expected_owner, action, request)
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

fn reconcile_mount_aliases(
    validated: &[PathBuf],
    devices: &[PathBuf],
    records: &[MountRecord],
) -> Vec<PathBuf> {
    let mut affected = validated.iter().cloned().collect::<BTreeSet<_>>();
    let mut sources = devices.iter().cloned().collect::<BTreeSet<_>>();
    loop {
        let mut changed = false;
        for record in records {
            let destination_is_affected = affected
                .iter()
                .any(|mount| record.destination().starts_with(mount));
            let source_is_affected = sources.contains(record.source())
                || affected
                    .iter()
                    .any(|mount| record.source().starts_with(mount));
            if destination_is_affected || source_is_affected {
                changed |= sources.insert(record.source().to_path_buf());
                changed |= affected.insert(record.destination().to_path_buf());
            }
        }
        if !changed {
            return affected.into_iter().collect();
        }
    }
}

fn reconcile_anonymous_mount_ids(
    mounts: &BTreeMap<PathBuf, MountRecord>,
    consumed: &BTreeSet<PathBuf>,
    prior: &[Volume],
) -> BTreeMap<PathBuf, VolumeId> {
    let prior_by_destination = prior
        .iter()
        .flat_map(|volume| {
            volume
                .mount_points()
                .iter()
                .map(move |destination| (destination.clone(), volume))
        })
        .collect::<BTreeMap<_, _>>();
    let mut reconciled = mounts
        .iter()
        .filter(|(destination, record)| {
            !consumed.contains(*destination) && !record.source().starts_with("/dev/")
        })
        .filter_map(|(destination, record)| {
            prior_by_destination
                .get(destination)
                .filter(|volume| volume.filesystem_type() == Some(record.filesystem_type()))
                .map(|volume| (destination.clone(), volume.id().clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut current_by_source = BTreeMap::<(PathBuf, Box<str>), Vec<PathBuf>>::new();
    for (destination, record) in mounts {
        if !consumed.contains(destination) && !record.source().starts_with("/dev/") {
            current_by_source
                .entry((
                    record.source().to_path_buf(),
                    record.filesystem_type().into(),
                ))
                .or_default()
                .push(destination.clone());
        }
    }
    for ((source, filesystem_type), destinations) in current_by_source {
        // Preserve unchanged destinations first. If none survive, a complete
        // source group can migrate by a deterministic destination pairing.
        if destinations
            .iter()
            .any(|destination| reconciled.contains_key(destination))
        {
            continue;
        }
        let mut previous = prior
            .iter()
            .filter(|volume| {
                volume.device() == source
                    && volume.filesystem_type() == Some(filesystem_type.as_ref())
            })
            .filter_map(|volume| {
                volume
                    .mount_points()
                    .first()
                    .map(|destination| (destination, volume.id()))
            })
            .collect::<Vec<_>>();
        previous.sort_by(|left, right| left.0.cmp(right.0));
        if previous.len() == destinations.len() {
            for (destination, (_, id)) in destinations.into_iter().zip(previous) {
                reconciled.insert(destination, id.clone());
            }
        }
    }
    reconciled
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
        return Err(VolumeError::ActionUnavailable);
    }
    if action == VolumeAction::Unlock && unlock_secret.is_none() {
        return Err(VolumeError::UnlockSecretRequired);
    }
    Ok(())
}
