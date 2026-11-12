//! Typed wrappers over the runtime's early-event channels.
//!
//! Most first-party plugins are not calls at all: the platform publishes
//! a snapshot (battery, network, theme, insets, …) on a latest-value
//! channel and Rust reads it. These types carry that shape once:
//!
//! - [`LatestValue`] ties a message type to its plugin id and channel.
//! - [`LatestClient`] reads the retained value and subscribes to updates.
//! - [`EarlyStream`] decodes a channel's raw payloads, latest-value or
//!   queue alike.
//! - [`LatestPublisher`] publishes from Rust, skipping unchanged values.
//!
//! Each plugin module names its instantiations (`BatteryStateClient`,
//! `BatteryStateStream`, …), so callers rarely spell these directly.

use std::fmt;
use std::marker::PhantomData;
use std::sync::{Arc, Weak};

use flume::Receiver;
use istmo_core::codec::{self, Decode, Encode};
use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Plugin, Runtime};

/// A message published on a latest-value early-event channel.
pub trait LatestValue {
    /// Id of the plugin the channel belongs to; checked against the
    /// runtime's declared plugins.
    const PLUGIN_ID: &'static str;
    /// Early-event channel the value is published on.
    const CHANNEL: &'static str;
}

/// Reader for the latest-value channel of `T`.
pub struct LatestClient<T> {
    slot: Arc<LatestValueSlot>,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for LatestClient<T> {
    fn clone(&self) -> Self {
        Self {
            slot: self.slot.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T> fmt::Debug for LatestClient<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LatestClient")
            .field("value", &std::any::type_name::<T>())
            .finish_non_exhaustive()
    }
}

impl<T: LatestValue> Plugin for LatestClient<T> {
    const PLUGIN_ID: &'static str = T::PLUGIN_ID;
}

impl<T: LatestValue> LatestClient<T> {
    /// Client bound to the process-wide runtime.
    ///
    /// # Errors
    ///
    /// Fails when no global runtime is installed or the plugin was not
    /// declared.
    pub fn acquire() -> Result<Self, IstmoError> {
        Self::from_runtime(&Runtime::global()?)
    }

    /// Client bound to `rt`.
    ///
    /// # Errors
    ///
    /// [`IstmoError::PluginNotDeclared`] when enforcement is on and the
    /// plugin was not declared.
    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        rt.check_declared(T::PLUGIN_ID)?;
        Ok(Self {
            slot: rt.early_events().latest_slot(T::CHANNEL),
            _marker: PhantomData,
        })
    }
}

impl<T: LatestValue + Decode> LatestClient<T> {
    /// The retained value, or `None` before the first publish.
    ///
    /// # Errors
    ///
    /// [`IstmoError::Codec`] when the retained payload does not decode.
    pub fn current(&self) -> Result<Option<T>, IstmoError> {
        self.slot.peek().map(|bytes| decode(&bytes)).transpose()
    }

    /// [`current`](Self::current), or `T::default()` before the first
    /// publish.
    ///
    /// # Errors
    ///
    /// Same as [`current`](Self::current).
    pub fn current_or_default(&self) -> Result<T, IstmoError>
    where
        T: Default,
    {
        Ok(self.current()?.unwrap_or_default())
    }

    /// Subscribe: yields the retained value first (if any), then every
    /// later publish.
    #[must_use]
    pub fn stream(&self) -> EarlyStream<T> {
        EarlyStream::new(self.slot.subscribe())
    }

    /// Former name of [`stream`](Self::stream).
    #[deprecated(note = "use `stream()`")]
    #[must_use]
    pub fn updates(&self) -> EarlyStream<T> {
        self.stream()
    }
}

/// Decoding receiver over one early-event channel.
pub struct EarlyStream<T> {
    rx: Receiver<Vec<u8>>,
    _marker: PhantomData<fn() -> T>,
}

impl<T> fmt::Debug for EarlyStream<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EarlyStream")
            .field("item", &std::any::type_name::<T>())
            .field("buffered", &self.rx.len())
            .finish()
    }
}

impl<T> EarlyStream<T> {
    pub(crate) const fn new(rx: Receiver<Vec<u8>>) -> Self {
        Self {
            rx,
            _marker: PhantomData,
        }
    }
}

impl<T: Decode> EarlyStream<T> {
    /// Block until the next value.
    ///
    /// # Errors
    ///
    /// [`IstmoError::ChannelClosed`] once the channel is gone, or
    /// [`IstmoError::Codec`] when the payload does not decode.
    pub fn recv(&self) -> Result<T, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Async variant of [`recv`](Self::recv).
    ///
    /// # Errors
    ///
    /// Same as [`recv`](Self::recv).
    pub async fn recv_async(&self) -> Result<T, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Next buffered value without blocking, `None` when nothing is
    /// buffered.
    #[must_use]
    pub fn try_recv(&self) -> Option<Result<T, IstmoError>> {
        self.rx.try_recv().ok().map(|bytes| decode(&bytes))
    }
}

/// Publishes `T` from Rust onto its latest-value channel, skipping
/// values equal to the last one published.
///
/// Holds the runtime weakly: once the runtime is gone, publishing is a
/// no-op.
#[derive(Debug)]
pub struct LatestPublisher<T> {
    runtime: Weak<Runtime>,
    last: Option<T>,
}

impl<T: LatestValue + Encode + PartialEq> LatestPublisher<T> {
    /// Publisher bound to `runtime`.
    ///
    /// # Errors
    ///
    /// [`IstmoError::PluginNotDeclared`] when enforcement is on and the
    /// plugin was not declared.
    pub fn install(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        runtime.check_declared(T::PLUGIN_ID)?;
        Ok(Self {
            runtime: Arc::downgrade(runtime),
            last: None,
        })
    }

    /// Publish `value` unless it equals the last published one. Returns
    /// whether it was published.
    pub fn publish_if_changed(&mut self, value: T) -> bool {
        if self.last.as_ref() == Some(&value) {
            return false;
        }
        let Some(runtime) = self.runtime.upgrade() else {
            return false;
        };
        match codec::encode(&value) {
            Ok(bytes) => {
                runtime.publish_early_latest(T::CHANNEL, bytes);
                self.last = Some(value);
                true
            }
            Err(err) => {
                tracing::warn!(target: "istmo.plugins", channel = T::CHANNEL, %err, "encode failed");
                false
            }
        }
    }

    /// Whether anything was published yet.
    #[must_use]
    pub const fn has_published(&self) -> bool {
        self.last.is_some()
    }

    /// The last published value.
    #[must_use]
    pub const fn last(&self) -> Option<&T> {
        self.last.as_ref()
    }
}

fn decode<T: Decode>(bytes: &[u8]) -> Result<T, IstmoError> {
    let (value, _) = codec::decode::<T>(bytes)?;
    Ok(value)
}
