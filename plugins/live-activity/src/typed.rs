//! Strongly-typed generic wrapper over [`LiveActivityClient`].
//!
//! The plugin trait is non-generic on the wire (bincode-encoded payloads
//! tagged by `activity_type` string). This wrapper adds a compile-time
//! type binding so callers hold `TypedLiveActivity<TimerAttributes,
//! TimerState>` and never touch raw bytes.
//!
//! One instance per activity type. Multiple types (`"timer"`,
//! `"delivery"`, `"workout"`) coexist in the same runtime by holding
//! multiple `TypedLiveActivity<A, C>` values.

use std::marker::PhantomData;
use std::sync::Arc;

use istmo_core::{IstmoError, Message, NativeHandle, NativeHandleId, Runtime, codec};

use crate::{
    ActivityError, ActivityStyle, AlertConfig, AndroidTierHint, DismissalPolicy,
    LiveActivityClient, LiveActivityToken, PlatformCapabilities, RestoredActivity,
};

/// Compile-time typed wrapper around [`LiveActivityClient`].
///
/// `A` is the immutable attributes struct attached at start time; `C` is
/// the mutable content state pushed with each update. Both must derive
/// `bincode::Encode` + `bincode::Decode` (the `#[istmo::message]` attribute
/// takes care of this).
pub struct TypedLiveActivity<A, C>
where
    A: Message,
    C: Message,
{
    client: LiveActivityClient,
    activity_type: String,
    _marker: PhantomData<fn() -> (A, C)>,
}

impl<A, C> TypedLiveActivity<A, C>
where
    A: Message,
    C: Message,
{
    /// Bind to the live-activity plugin for the given `activity_type`.
    ///
    /// The string identifies which native-side backend handles this
    /// type — Kotlin `IstmoRuntime.registerLiveActivityBackend(id, …)` /
    /// Swift `IstmoRuntime.shared.register(activityBackend:for:)` must
    /// have been called with a matching id.
    pub fn new(runtime: &Arc<Runtime>, activity_type: impl Into<String>) -> Result<Self, IstmoError> {
        Ok(Self {
            client: LiveActivityClient::from_runtime(runtime)?,
            activity_type: activity_type.into(),
            _marker: PhantomData,
        })
    }

    /// Wire-level activity-type identifier this wrapper is bound to.
    #[must_use]
    pub fn activity_type(&self) -> &str {
        &self.activity_type
    }

    /// Handle to the shared runtime powering the underlying client.
    #[must_use]
    pub fn runtime(&self) -> &Arc<Runtime> {
        self.client.runtime()
    }

    /// Start a new live activity, returning a RAII [`NativeHandle`]. When
    /// dropped, the handle fires `Frame::ReleaseNativeHandle` which the
    /// native side treats as an immediate end.
    pub async fn start(
        &self,
        attributes: A,
        initial_state: C,
        style: ActivityStyle,
    ) -> Result<NativeHandle<LiveActivityToken>, ActivityError> {
        self.start_with_hint(attributes, initial_state, style, None, None).await
    }

    /// Full-featured start with optional stale-after / Android tier hint.
    pub async fn start_with_hint(
        &self,
        attributes: A,
        initial_state: C,
        style: ActivityStyle,
        stale_after_seconds: Option<u32>,
        android_tier_hint: Option<AndroidTierHint>,
    ) -> Result<NativeHandle<LiveActivityToken>, ActivityError> {
        let attributes = codec::encode(&attributes).map_err(|e| ActivityError::Decode(e.to_string()))?;
        let initial_state = codec::encode(&initial_state).map_err(|e| ActivityError::Decode(e.to_string()))?;

        let id = self
            .call_start(
                self.activity_type.clone(),
                attributes,
                initial_state,
                style,
                stale_after_seconds,
                android_tier_hint,
            )
            .await?;
        Ok(NativeHandle::adopt(self.runtime(), id))
    }

    /// Push a new state snapshot. `alert` opts into a heads-up presentation.
    pub async fn update(
        &self,
        handle: NativeHandleId,
        state: C,
        alert: Option<AlertConfig>,
    ) -> Result<(), ActivityError> {
        let state = codec::encode(&state).map_err(|e| ActivityError::Decode(e.to_string()))?;
        self.call_update(handle, state, alert).await
    }

    /// End the activity. `final_state` optionally supplies a last snapshot
    /// to display through the dismissal grace window (iOS only).
    pub async fn end(
        &self,
        handle: NativeHandleId,
        final_state: Option<C>,
        dismissal: DismissalPolicy,
    ) -> Result<(), ActivityError> {
        let final_state = match final_state {
            Some(state) => Some(codec::encode(&state).map_err(|e| ActivityError::Decode(e.to_string()))?),
            None => None,
        };
        self.call_end(handle, final_state, dismissal).await
    }

    /// Probe whether the platform is willing to accept new activities.
    pub async fn are_activities_enabled(&self) -> Result<bool, ActivityError> {
        translate(self.client.are_activities_enabled().await)
    }

    /// Full capability snapshot for the current device / OS version.
    pub async fn capabilities(&self) -> Result<PlatformCapabilities, ActivityError> {
        translate(self.client.capabilities().await)
    }

    /// Reattach to previously-started activities of this type that
    /// survived a process restart. Returns a tuple of `(NativeHandle,
    /// attributes, latest_state)` per restored activity.
    ///
    /// Activities of other types are filtered out — the caller holds one
    /// `TypedLiveActivity` per type and calls `restore` on each.
    pub async fn restore(&self) -> Result<Vec<Restored<A, C>>, ActivityError> {
        let all: Vec<RestoredActivity> = translate(self.client.restore_active().await)?;
        let mut out = Vec::new();
        for r in all {
            if r.activity_type != self.activity_type {
                continue;
            }
            let (attributes, _) = codec::decode::<A>(&r.attributes)
                .map_err(|e| ActivityError::Decode(e.to_string()))?;
            let (state, _) = codec::decode::<C>(&r.state)
                .map_err(|e| ActivityError::Decode(e.to_string()))?;
            out.push(Restored {
                handle: NativeHandle::adopt(self.runtime(), r.handle),
                attributes,
                state,
            });
        }
        Ok(out)
    }

    // ---- internal wire adapters ----

    async fn call_start(
        &self,
        activity_type: String,
        attributes: Vec<u8>,
        initial_state: Vec<u8>,
        style: ActivityStyle,
        stale_after_seconds: Option<u32>,
        android_tier_hint: Option<AndroidTierHint>,
    ) -> Result<NativeHandleId, ActivityError> {
        translate(
            self.client
                .start(
                    activity_type,
                    attributes,
                    initial_state,
                    style,
                    stale_after_seconds,
                    android_tier_hint,
                )
                .await,
        )
    }

    async fn call_update(
        &self,
        handle: NativeHandleId,
        state: Vec<u8>,
        alert: Option<AlertConfig>,
    ) -> Result<(), ActivityError> {
        translate(self.client.update(handle, state, alert).await)
    }

    async fn call_end(
        &self,
        handle: NativeHandleId,
        final_state: Option<Vec<u8>>,
        dismissal: DismissalPolicy,
    ) -> Result<(), ActivityError> {
        translate(self.client.end(handle, final_state, dismissal).await)
    }
}

/// A restored activity produced by [`TypedLiveActivity::restore`]. The
/// [`NativeHandle`] is already adopted; drop or hold as needed.
pub struct Restored<A, C>
where
    A: Message,
    C: Message,
{
    pub handle: NativeHandle<LiveActivityToken>,
    pub attributes: A,
    pub state: C,
}

impl<A, C> std::fmt::Debug for Restored<A, C>
where
    A: Message + std::fmt::Debug,
    C: Message + std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Restored")
            .field("handle", &self.handle.id())
            .field("attributes", &self.attributes)
            .field("state", &self.state)
            .finish()
    }
}

/// Convert an `IstmoError` returned by the generated client into the
/// plugin-specific [`ActivityError`]. The generated `PluginError { bytes }`
/// carries the bincode-encoded `ActivityError` produced by the native
/// backend; any other transport-level error surfaces as `Backend(...)`.
fn translate<T>(result: Result<T, IstmoError>) -> Result<T, ActivityError> {
    match result {
        Ok(v) => Ok(v),
        Err(IstmoError::PluginError { bytes }) => match codec::decode::<ActivityError>(&bytes) {
            Ok((decoded, _)) => Err(decoded),
            Err(e) => Err(ActivityError::Backend(format!(
                "failed to decode plugin error: {e}"
            ))),
        },
        Err(other) => Err(ActivityError::Backend(other.to_string())),
    }
}

impl<A, C> std::fmt::Debug for TypedLiveActivity<A, C>
where
    A: Message,
    C: Message,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypedLiveActivity")
            .field("activity_type", &self.activity_type)
            .field("attributes", &std::any::type_name::<A>())
            .field("state", &std::any::type_name::<C>())
            .finish()
    }
}
