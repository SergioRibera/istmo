//! DRM timeline syncobjs: PipeWire explicit sync (`SPA_META_SyncTimeline`).
//!
//! A producer that negotiates explicit sync adds an acquire and a release
//! timeline syncobj to every buffer. The plugin waits for the acquire
//! point on the CPU before a frame leaves (so frames are `Ready` and no
//! consumer handles syncobjs: the data plane never carries a timeline)
//! and signals the release point every time the buffer goes back, read
//! or not. This is the only synchronisation that holds on NVIDIA, whose
//! driver attaches no implicit fences.
//!
//! The ioctls are core DRM, so any render node waits on a syncobj
//! another process exported; the producer's node is preferred.

use std::collections::HashMap;
use std::fs::File;
use std::hash::Hash;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::MetadataExt as _;
use std::sync::Arc;
use std::time::Duration;

use drm::control::{Device as _, syncobj};

/// How long a producer's acquire point is waited for before the frame is
/// dropped. A compositor finishes a frame well within this.
pub(crate) const ACQUIRE_TIMEOUT: Duration = Duration::from_millis(100);

/// A render node opened for syncobj ioctls.
#[derive(Debug)]
struct Node(File);

impl AsFd for Node {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Node {}
impl drm::control::Device for Node {}

/// What a wait saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Waited {
    Signalled,
    TimedOut,
}

/// An open DRM node. Cheap to clone.
#[derive(Debug, Clone)]
pub(crate) struct SyncDevice {
    node: Arc<Node>,
}

impl SyncDevice {
    /// The render node of `dev_t` when given and present, else the first
    /// render node that opens.
    ///
    /// # Errors
    /// `NotFound` when no render node opens.
    pub(crate) fn open(dev_t: Option<u64>) -> std::io::Result<Self> {
        let nodes: Vec<String> = (128..=192)
            .map(|n| format!("/dev/dri/renderD{n}"))
            .filter(|p| std::path::Path::new(p).exists())
            .collect();
        let open = |path: &String| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .ok()
        };
        let wanted = dev_t.and_then(|dev| {
            nodes
                .iter()
                .find(|p| std::fs::metadata(p).is_ok_and(|m| m.rdev() == dev))
                .and_then(open)
        });
        let file = wanted
            .or_else(|| nodes.iter().find_map(open))
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no render node"))?;
        Ok(Self {
            node: Arc::new(Node(file)),
        })
    }

    /// Import a syncobj another process exported. The fd stays the
    /// caller's.
    ///
    /// # Errors
    /// The ioctl's error (`EINVAL` for an fd that is no syncobj).
    pub(crate) fn import(&self, fd: BorrowedFd<'_>) -> std::io::Result<Syncobj> {
        let handle = self.node.fd_to_syncobj(fd, false)?;
        Ok(Syncobj {
            node: Arc::clone(&self.node),
            handle,
        })
    }

    /// A new timeline syncobj (tests stand in for a producer with it).
    #[cfg(test)]
    pub(crate) fn create(&self) -> std::io::Result<Syncobj> {
        let handle = self.node.create_syncobj(false)?;
        Ok(Syncobj {
            node: Arc::clone(&self.node),
            handle,
        })
    }
}

/// One imported syncobj handle; destroyed on drop.
#[derive(Debug)]
pub(crate) struct Syncobj {
    node: Arc<Node>,
    handle: syncobj::Handle,
}

impl Syncobj {
    /// Wait until timeline `point` was submitted and signalled, or
    /// `timeout` passed.
    ///
    /// # Errors
    /// The ioctl's error other than the timeout.
    pub(crate) fn wait(&self, point: u64, timeout: Duration) -> std::io::Result<Waited> {
        match self.node.syncobj_timeline_wait(
            &[self.handle],
            &[point],
            deadline(timeout),
            true,
            true,
            false,
        ) {
            Ok(_) => Ok(Waited::Signalled),
            Err(err) if err.raw_os_error() == Some(rustix::io::Errno::TIME.raw_os_error()) => {
                Ok(Waited::TimedOut)
            }
            Err(err) => Err(err),
        }
    }

    /// Signal timeline `point` from the CPU (the consumer's release).
    ///
    /// # Errors
    /// The ioctl's error.
    pub(crate) fn signal(&self, point: u64) -> std::io::Result<()> {
        self.node.syncobj_timeline_signal(&[self.handle], &[point])
    }

    #[cfg(test)]
    pub(crate) fn export(&self) -> std::io::Result<std::os::fd::OwnedFd> {
        self.node.syncobj_to_fd(self.handle, false)
    }
}

impl Drop for Syncobj {
    fn drop(&mut self) {
        if let Err(err) = self.node.destroy_syncobj(self.handle) {
            tracing::warn!(%err, "screen-capture: destroying a DRM syncobj handle");
        }
    }
}

/// `CLOCK_MONOTONIC` now + `timeout` in nanoseconds: the absolute
/// deadline DRM waits take, saturating.
fn deadline(timeout: Duration) -> i64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let now_ns = now
        .tv_sec
        .saturating_mul(1_000_000_000)
        .saturating_add(now.tv_nsec);
    now_ns.saturating_add(i64::try_from(timeout.as_nanos()).unwrap_or(i64::MAX))
}

/// The acquire and release syncobjs of one buffer, as the producer
/// attached them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TimelineFds<'b> {
    pub(crate) acquire: BorrowedFd<'b>,
    pub(crate) release: BorrowedFd<'b>,
}

/// Waits on acquire points and signals release points of one stream's
/// buffers, opening the node at the first synchronised buffer. Each
/// buffer's syncobjs are imported once, at its first use, and kept until
/// [`forget`](Self::forget).
#[derive(Debug)]
pub(crate) struct ExplicitSync<K> {
    producer: Option<u64>,
    device: Option<SyncDevice>,
    imported: HashMap<K, Imported>,
}

#[derive(Debug)]
struct Imported {
    acquire: Arc<Syncobj>,
    release: Syncobj,
}

/// Whether a buffer's acquire point signalled.
#[derive(Debug)]
pub(crate) enum Acquire {
    Ready,
    /// Not yet: [`AcquireWait::wait`] it off the loop thread.
    Pending(AcquireWait),
}

/// An acquire point still to signal. `Send`: waited for on a thread of
/// its own, so a slow producer never blocks the loop's other streams.
#[derive(Debug)]
pub(crate) struct AcquireWait {
    syncobj: Arc<Syncobj>,
    point: u64,
}

impl AcquireWait {
    /// Block until the point signals, at most [`ACQUIRE_TIMEOUT`].
    ///
    /// # Errors
    /// The wait failed or timed out.
    pub(crate) fn wait(&self) -> Result<(), SyncError> {
        match self
            .syncobj
            .wait(self.point, ACQUIRE_TIMEOUT)
            .map_err(SyncError::Wait)?
        {
            Waited::Signalled => Ok(()),
            Waited::TimedOut => Err(SyncError::TimedOut),
        }
    }
}
/// Why a synchronised buffer cannot be read, or handed back.
#[derive(Debug)]
pub(crate) enum SyncError {
    Device(std::io::Error),
    Import(std::io::Error),
    Wait(std::io::Error),
    TimedOut,
    Signal(std::io::Error),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Device(err) => write!(f, "no DRM node to wait on syncobjs: {err}"),
            Self::Import(err) => write!(f, "importing a syncobj: {err}"),
            Self::Wait(err) => write!(f, "waiting for the acquire point: {err}"),
            Self::TimedOut => write!(
                f,
                "the acquire point did not signal within {ACQUIRE_TIMEOUT:?}"
            ),
            Self::Signal(err) => write!(f, "signalling the release point: {err}"),
        }
    }
}

impl<K: Copy + Eq + Hash> ExplicitSync<K> {
    /// Waits through the render node of `producer` when known.
    pub(crate) fn new(producer: Option<u64>) -> Self {
        Self {
            producer,
            device: None,
            imported: HashMap::new(),
        }
    }

    fn device(&mut self) -> Result<&SyncDevice, SyncError> {
        if self.device.is_none() {
            self.device = Some(SyncDevice::open(self.producer).map_err(SyncError::Device)?);
        }
        self.device
            .as_ref()
            .ok_or_else(|| SyncError::Device(std::io::ErrorKind::NotFound.into()))
    }

    /// The syncobjs of buffer `key`, imported at the first call.
    fn imported(&mut self, key: K, fds: TimelineFds<'_>) -> Result<&Imported, SyncError> {
        if !self.imported.contains_key(&key) {
            let device = self.device()?;
            let acquire = device.import(fds.acquire).map_err(SyncError::Import)?;
            let release = device.import(fds.release).map_err(SyncError::Import)?;
            self.imported.insert(
                key,
                Imported {
                    acquire: Arc::new(acquire),
                    release,
                },
            );
        }
        self.imported
            .get(&key)
            .ok_or_else(|| SyncError::Import(std::io::ErrorKind::NotFound.into()))
    }

    /// Whether buffer `key`'s acquire point already signalled (the
    /// producer finished rendering); never blocks.
    ///
    /// # Errors
    /// The node, the import or the check failed.
    pub(crate) fn poll(
        &mut self,
        key: K,
        fds: TimelineFds<'_>,
        point: u64,
    ) -> Result<Acquire, SyncError> {
        let acquire = Arc::clone(&self.imported(key, fds)?.acquire);
        match acquire
            .wait(point, Duration::ZERO)
            .map_err(SyncError::Wait)?
        {
            Waited::Signalled => Ok(Acquire::Ready),
            Waited::TimedOut => Ok(Acquire::Pending(AcquireWait {
                syncobj: acquire,
                point,
            })),
        }
    }

    /// Signal buffer `key`'s release point: nothing reads it any more.
    ///
    /// # Errors
    /// The node, the import or the signal failed.
    pub(crate) fn release(
        &mut self,
        key: K,
        fds: TimelineFds<'_>,
        point: u64,
    ) -> Result<(), SyncError> {
        self.imported(key, fds)?
            .release
            .signal(point)
            .map_err(SyncError::Signal)
    }

    /// Buffer `key` is gone: drop its imported syncobjs.
    pub(crate) fn forget(&mut self, key: &K) {
        self.imported.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    pub(crate) fn device() -> Option<SyncDevice> {
        SyncDevice::open(None)
            .map_err(|err| eprintln!("skipping syncobj test: {err}"))
            .ok()
    }

    #[test]
    fn wait_blocks_until_another_handle_signals() {
        let Some(device) = device() else { return };
        let Ok(timeline) = device.create() else {
            eprintln!("skipping: no timeline syncobjs on this node");
            return;
        };
        assert_eq!(
            timeline.wait(1, Duration::from_millis(20)).expect("wait"),
            Waited::TimedOut
        );
        let exported = timeline.export().expect("export");
        let producer = device.import(exported.as_fd()).expect("import");
        let start = Instant::now();
        let signaller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            producer.signal(1).expect("signal");
        });
        assert_eq!(
            timeline.wait(1, Duration::from_secs(2)).expect("wait"),
            Waited::Signalled
        );
        assert!(start.elapsed() >= Duration::from_millis(140));
        signaller.join().expect("signaller");
    }

    #[test]
    fn poll_never_blocks_and_release_reaches_the_producer() {
        let Some(device) = device() else { return };
        let (Ok(acquire), Ok(release)) = (device.create(), device.create()) else {
            return;
        };
        let (a, r) = (acquire.export().expect("a"), release.export().expect("r"));
        let fds = TimelineFds {
            acquire: a.as_fd(),
            release: r.as_fd(),
        };
        let mut sync = ExplicitSync::new(None);
        let start = Instant::now();
        let Ok(Acquire::Pending(wait)) = sync.poll(1, fds, 3) else {
            panic!("an unsignalled point is pending");
        };
        assert!(start.elapsed() < ACQUIRE_TIMEOUT, "poll does not wait");
        let waiter = std::thread::spawn(move || wait.wait());
        acquire.signal(3).expect("producer finished");
        waiter
            .join()
            .expect("waiter")
            .expect("signalled within the timeout");
        assert!(matches!(sync.poll(1, fds, 3), Ok(Acquire::Ready)));
        sync.release(1, fds, 4).expect("released");
        assert_eq!(
            release.wait(4, Duration::from_millis(100)).expect("wait"),
            Waited::Signalled,
            "the release point reaches the producer"
        );
        assert_eq!(sync.imported.len(), 1, "imported once per buffer");
        sync.forget(&1);
        assert!(sync.imported.is_empty());
        let Ok(Acquire::Pending(late)) = sync.poll(2, fds, 9) else {
            panic!("pending");
        };
        assert!(matches!(late.wait(), Err(SyncError::TimedOut)));
    }
}
