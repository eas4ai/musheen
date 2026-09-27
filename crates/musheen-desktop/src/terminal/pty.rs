use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use musheen_core::StorePath;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use super::{TerminalError, TerminalProfile, TerminalSize};

// At most 2 MiB of PTY output can wait for a slow UI consumer.
const MAX_PENDING_PTY_EVENTS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalExit {
    Code(u32),
    Signal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PtyEvent {
    Output(Vec<u8>),
    Exited(TerminalExit),
    ReadFailed,
}

pub struct TerminalSession {
    profile: TerminalProfile,
    cwd: StorePath,
    size: TerminalSize,
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
    receiver: async_channel::Receiver<PtyEvent>,
    running: Arc<AtomicBool>,
    child_pid: Option<u32>,
}

impl TerminalSession {
    pub fn spawn(
        profile: TerminalProfile,
        cwd: &StorePath,
        size: TerminalSize,
    ) -> Result<Self, TerminalError> {
        Self::spawn_inner(profile, cwd.clone(), size)
    }

    fn spawn_inner(
        profile: TerminalProfile,
        cwd: StorePath,
        size: TerminalSize,
    ) -> Result<Self, TerminalError> {
        let launch = profile.prepare(&cwd)?;
        let system = native_pty_system();
        let pair = system
            .openpty(to_pty_size(size))
            .map_err(|error| TerminalError::Spawn(error.to_string().into()))?;
        let mut command = CommandBuilder::new(launch.program());
        command.args(launch.arguments());
        command.cwd(
            launch
                .working_directory()
                .expect("terminal launches always have a working directory"),
        );
        command.env("TERM", "xterm-256color");
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| TerminalError::Spawn(error.to_string().into()))?;
        drop(pair.slave);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| TerminalError::Io(error.to_string().into()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| TerminalError::Io(error.to_string().into()))?;
        let child_pid = child.process_id();
        let killer = child.clone_killer();
        let (sender, receiver) = async_channel::bounded(MAX_PENDING_PTY_EVENTS);
        let running = Arc::new(AtomicBool::new(true));
        spawn_reader(reader, sender.clone());
        let wait_running = Arc::clone(&running);
        std::thread::Builder::new()
            .name("musheen-terminal-wait".into())
            .spawn(move || {
                let event = match child.wait() {
                    Ok(status) if status.signal().is_some() => {
                        PtyEvent::Exited(TerminalExit::Signal)
                    }
                    Ok(status) => PtyEvent::Exited(TerminalExit::Code(status.exit_code())),
                    Err(_) => PtyEvent::ReadFailed,
                };
                wait_running.store(false, Ordering::Release);
                let _ = sender.send_blocking(event);
            })
            .map_err(|error| TerminalError::Spawn(error.to_string().into()))?;
        Ok(Self {
            profile,
            cwd,
            size,
            master: pair.master,
            writer: Arc::new(Mutex::new(writer)),
            killer: Arc::new(Mutex::new(killer)),
            receiver,
            running,
            child_pid,
        })
    }

    pub fn write(&self, bytes: &[u8]) -> Result<(), TerminalError> {
        if !self.is_running() {
            return Err(TerminalError::NotRunning);
        }
        self.writer
            .lock()
            .map_err(|_| TerminalError::Io("terminal writer lock is unavailable".into()))?
            .write_all(bytes)
            .map_err(|error| TerminalError::Io(error.to_string().into()))
    }

    pub fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalError> {
        self.master
            .resize(to_pty_size(size))
            .map_err(|error| TerminalError::Io(error.to_string().into()))?;
        self.size = size;
        Ok(())
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<PtyEvent, TerminalError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.receiver.try_recv() {
                Ok(event) => return Ok(event),
                Err(async_channel::TryRecvError::Closed) => return Err(TerminalError::NotRunning),
                Err(async_channel::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(async_channel::TryRecvError::Empty) => {
                    return Err(TerminalError::Io("terminal event receive timed out".into()));
                }
            }
        }
    }

    #[must_use]
    pub fn events(&self) -> async_channel::Receiver<PtyEvent> {
        self.receiver.clone()
    }

    pub fn terminate(&mut self) -> Result<(), TerminalError> {
        if !self.running.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        self.killer
            .lock()
            .map_err(|_| TerminalError::Io("terminal child lock is unavailable".into()))?
            .kill()
            .map_err(|error| TerminalError::Io(error.to_string().into()))
    }

    /// Stops the child: a hangup first, as closing a terminal sends; then,
    /// if the child still leads its process group two seconds later, SIGKILL
    /// to that group, so a program that ignores the hangup does not keep
    /// running out of view.
    pub fn stop(&mut self) {
        let _ = self.terminate();
        let Some(pid) = self.child_pid.and_then(|pid| i32::try_from(pid).ok()) else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("musheen-terminal-stop".to_owned())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(2));
                if leads_live_process_group(pid)
                    && let Some(group) = rustix::process::Pid::from_raw(pid)
                {
                    let _ =
                        rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
                }
            });
    }

    /// The child's process ID, while it is known.
    #[must_use]
    pub const fn process_id(&self) -> Option<u32> {
        self.child_pid
    }

    pub fn restart(&mut self) -> Result<(), TerminalError> {
        let _ = self.terminate();
        let replacement = Self::spawn_inner(self.profile.clone(), self.cwd.clone(), self.size)?;
        *self = replacement;
        Ok(())
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Returns whether the TTY foreground process group differs from the shell.
    /// `/proc` avoids borrowing the PTY's raw descriptor through unsafe code.
    #[must_use]
    pub fn has_foreground_job(&self) -> bool {
        let Some(pid) = self.child_pid else {
            return false;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            return false;
        };
        let fields = fields.split_whitespace().collect::<Vec<_>>();
        let process_group = fields.get(2).and_then(|value| value.parse::<i64>().ok());
        let foreground_group = fields.get(5).and_then(|value| value.parse::<i64>().ok());
        matches!((process_group, foreground_group), (Some(shell), Some(active)) if active > 0 && active != shell)
    }
}

/// Whether process `pid` still runs, not yet reaped or a zombie, and leads its
/// own process group, so its group may be signalled without reaching a
/// process that took over a reused ID.
fn leads_live_process_group(pid: i32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, fields)) = stat.rsplit_once(") ") else {
        return false;
    };
    let fields = fields.split_whitespace().collect::<Vec<_>>();
    fields.first() != Some(&"Z")
        && fields.get(2).and_then(|group| group.parse::<i32>().ok()) == Some(pid)
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

fn spawn_reader(mut reader: Box<dyn Read + Send>, sender: async_channel::Sender<PtyEvent>) {
    let _ = std::thread::Builder::new()
        .name("musheen-terminal-read".into())
        .spawn(move || {
            let mut buffer = vec![0; 32 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        if sender
                            .send_blocking(PtyEvent::Output(buffer[..count].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        let _ = sender.send_blocking(PtyEvent::ReadFailed);
                        break;
                    }
                }
            }
        });
}

const fn to_pty_size(size: TerminalSize) -> PtySize {
    PtySize {
        rows: size.rows(),
        cols: size.columns(),
        pixel_width: size.cell_width(),
        pixel_height: size.cell_height(),
    }
}
