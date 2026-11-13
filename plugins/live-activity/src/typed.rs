use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use istmo_core::sync::lock;
use istmo_core::{IstmoError, Message, NativeHandle, NativeHandleId, Runtime, codec};

use crate::{
    ActivityError, ActivityStyle, AlertConfig, AndroidTierHint, DismissalPolicy,
    LiveActivityClient, LiveActivityToken, PlatformCapabilities, RestoredActivity,
};

/// [`LiveActivityClient`] bound to one `activity_type`, with the
/// attributes (`A`) and content-state (`C`) types fixed at compile time.
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
    pub fn new(
        runtime: &Arc<Runtime>,
        activity_type: impl Into<String>,
    ) -> Result<Self, IstmoError> {
        Ok(Self {
            client: LiveActivityClient::from_runtime(runtime)?,
            activity_type: activity_type.into(),
            _marker: PhantomData,
        })
    }

    #[must_use]
    pub fn activity_type(&self) -> &str {
        &self.activity_type
    }

    #[must_use]
    pub const fn runtime(&self) -> &Arc<Runtime> {
        self.client.runtime()
    }

    pub async fn start(
        &self,
        attributes: A,
        initial_state: C,
        style: ActivityStyle,
    ) -> Result<NativeHandle<LiveActivityToken>, ActivityError> {
        self.start_with_hint(attributes, initial_state, style, None, None)
            .await
    }

    pub async fn start_with_hint(
        &self,
        attributes: A,
        initial_state: C,
        style: ActivityStyle,
        stale_after_seconds: Option<u32>,
        android_tier_hint: Option<AndroidTierHint>,
    ) -> Result<NativeHandle<LiveActivityToken>, ActivityError> {
        let attributes =
            codec::encode(&attributes).map_err(|e| ActivityError::Decode(e.to_string()))?;
        let initial_state =
            codec::encode(&initial_state).map_err(|e| ActivityError::Decode(e.to_string()))?;

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
        // A later `restore` may report this activity under the same id.
        claim(self.runtime(), id);
        Ok(NativeHandle::adopt(self.runtime(), id))
    }

    /// Push a new content state to the activity behind `handle`.
    pub async fn update(
        &self,
        handle: &NativeHandle<LiveActivityToken>,
        state: C,
        alert: Option<AlertConfig>,
    ) -> Result<(), ActivityError> {
        self.update_by_id(handle.id(), state, alert).await
    }

    /// [`Self::update`] for callers that only kept the raw id, e.g.
    /// because the owning [`NativeHandle`] lives behind a lock.
    pub async fn update_by_id(
        &self,
        handle: NativeHandleId,
        state: C,
        alert: Option<AlertConfig>,
    ) -> Result<(), ActivityError> {
        let state = codec::encode(&state).map_err(|e| ActivityError::Decode(e.to_string()))?;
        self.call_update(handle, state, alert).await
    }

    /// End the activity behind `handle`.
    ///
    /// Dropping `handle` afterwards is harmless: the native side has
    /// already forgotten the id and ignores the release.
    pub async fn end(
        &self,
        handle: &NativeHandle<LiveActivityToken>,
        final_state: Option<C>,
        dismissal: DismissalPolicy,
    ) -> Result<(), ActivityError> {
        self.end_by_id(handle.id(), final_state, dismissal).await
    }

    /// [`Self::end`] for callers that only kept the raw id.
    pub async fn end_by_id(
        &self,
        handle: NativeHandleId,
        final_state: Option<C>,
        dismissal: DismissalPolicy,
    ) -> Result<(), ActivityError> {
        let final_state = final_state
            .map(|state| codec::encode(&state).map_err(|e| ActivityError::Decode(e.to_string())))
            .transpose()?;
        self.call_end(handle, final_state, dismissal).await
    }

    pub async fn are_activities_enabled(&self) -> Result<bool, ActivityError> {
        translate(self.client.are_activities_enabled().await)
    }

    pub async fn capabilities(&self) -> Result<PlatformCapabilities, ActivityError> {
        translate(self.client.capabilities().await)
    }

    /// Recover the activities of this adapter's `activity_type` that
    /// outlived a previous process.
    ///
    /// The backend only hands out handles for this `activity_type`, and
    /// hands out the same handle again for an activity it already
    /// reported. A handle this process already owns — from an earlier
    /// `restore` or from [`Self::start`] — is left out, so calling it
    /// again never yields a second owner whose drop would end an
    /// activity the first one still uses.
    ///
    /// An activity whose payload no longer decodes (e.g. after a schema
    /// change), or that a misbehaving backend reports under another
    /// type, is skipped with a warning and *not* adopted: dropping an
    /// owning handle ends the activity, and a skipped one is left
    /// running instead (the platform dismisses it eventually).
    pub async fn restore(&self) -> Result<Vec<Restored<A, C>>, ActivityError> {
        let all: Vec<RestoredActivity> =
            translate(self.client.restore_active(self.activity_type.clone()).await)?;

        let mut out = Vec::with_capacity(all.len());
        for RestoredActivity {
            handle,
            activity_type,
            attributes,
            state,
        } in all
        {
            if activity_type != self.activity_type {
                tracing::warn!(
                    expected = %self.activity_type,
                    got = %activity_type,
                    handle = handle.0,
                    "skipping restored live activity of another type",
                );
                continue;
            }
            let decoded = codec::decode::<A>(&attributes).and_then(|(attributes, _)| {
                codec::decode::<C>(&state).map(|(s, _)| (attributes, s))
            });
            match decoded {
                Ok((attributes, state)) => {
                    if !claim(self.runtime(), handle) {
                        continue;
                    }
                    out.push(Restored {
                        handle: NativeHandle::adopt(self.runtime(), handle),
                        attributes,
                        state,
                    });
                }
                Err(err) => tracing::warn!(
                    activity_type = %self.activity_type,
                    handle = handle.0,
                    %err,
                    "skipping restored live activity with undecodable payload",
                ),
            }
        }
        Ok(out)
    }

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

/// A restored activity with its payloads decoded, returned by
/// [`TypedLiveActivity::restore`].
pub struct Restored<A, C>
where
    A: Message,
    C: Message,
{
    /// Owning handle; dropping it ends the platform activity.
    pub handle: NativeHandle<LiveActivityToken>,
    /// Attributes as passed to `start()`.
    pub attributes: A,
    /// Latest content state observed for the activity.
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

/// Record that this process owns `id` on `runtime`; `false` when it
/// already did.
///
/// Handle ids are never reused by a runtime, so ids whose owner has
/// since been dropped or ended can stay recorded: the backend no longer
/// reports them. Kept per runtime, since ids are allocated per runtime;
/// entries of dropped runtimes are discarded first, so a new runtime at a
/// reused address starts clean.
fn claim(runtime: &Arc<Runtime>, id: NativeHandleId) -> bool {
    type Claimed = Vec<(Weak<Runtime>, HashSet<u64>)>;
    static CLAIMED: OnceLock<Mutex<Claimed>> = OnceLock::new();
    let mut claimed = lock(CLAIMED.get_or_init(Mutex::default));
    claimed.retain(|(rt, _)| rt.strong_count() > 0);
    let found = claimed
        .iter()
        .position(|(rt, _)| std::ptr::eq(rt.as_ptr(), Arc::as_ptr(runtime)));
    let at = found.unwrap_or_else(|| {
        claimed.push((Arc::downgrade(runtime), HashSet::new()));
        claimed.len() - 1
    });
    claimed[at].1.insert(id.0)
}

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
            .finish_non_exhaustive()
    }
}
