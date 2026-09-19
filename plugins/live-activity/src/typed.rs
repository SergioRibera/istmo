use std::marker::PhantomData;
use std::sync::Arc;

use istmo_core::{IstmoError, Message, NativeHandle, NativeHandleId, Runtime, codec};

use crate::{
    ActivityError, ActivityStyle, AlertConfig, AndroidTierHint, DismissalPolicy,
    LiveActivityClient, LiveActivityToken, PlatformCapabilities, RestoredActivity,
};

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

    pub fn new(runtime: &Arc<Runtime>, activity_type: impl Into<String>) -> Result<Self, IstmoError> {
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
    pub fn runtime(&self) -> &Arc<Runtime> {
        self.client.runtime()
    }

    pub async fn start(
        &self,
        attributes: A,
        initial_state: C,
        style: ActivityStyle,
    ) -> Result<NativeHandle<LiveActivityToken>, ActivityError> {
        self.start_with_hint(attributes, initial_state, style, None, None).await
    }

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

    pub async fn update(
        &self,
        handle: NativeHandleId,
        state: C,
        alert: Option<AlertConfig>,
    ) -> Result<(), ActivityError> {
        let state = codec::encode(&state).map_err(|e| ActivityError::Decode(e.to_string()))?;
        self.call_update(handle, state, alert).await
    }

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

    pub async fn are_activities_enabled(&self) -> Result<bool, ActivityError> {
        translate(self.client.are_activities_enabled().await)
    }

    pub async fn capabilities(&self) -> Result<PlatformCapabilities, ActivityError> {
        translate(self.client.capabilities().await)
    }

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

