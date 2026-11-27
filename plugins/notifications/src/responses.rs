//! The responses lane: what the user did with a notification.
//!
//! Hosts publish every [`NotificationResponse`] on the early-event queue
//! [`RESPONSES_CHANNEL`] (capacity [`RESPONSES_CAPACITY`]), so a tap that
//! started the app waits until the app subscribes. A response with a URL
//! is also published as a `DeepLink` on `istmo.deeplinks`, with source
//! `notification:<id>` or `notification:<id>:<action>`.
//!
//! Native shells publish through `Frame::EarlyEvent` on the same
//! channel (the generated `NotificationResponse` codec), Rust hosts
//! through [`NotificationResponses::publish`].

use std::marker::PhantomData;
use std::sync::Arc;

use istmo_core::early_events::PreMainQueue;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_plugins::{DEEPLINKS_CHANNEL, DEFAULT_DEEPLINKS_CAPACITY, DeepLink};

use crate::{NOTIFICATIONS_PLUGIN_ID, NotificationResponse, ResponseAction};

/// Early-event channel responses are published on.
pub const RESPONSES_CHANNEL: &str = "istmo.notifications.responses";

/// Responses kept before the first subscriber.
pub const RESPONSES_CAPACITY: usize = 64;

/// Prefix of `DeepLink::source` for links published from a response.
const SOURCE_PREFIX: &str = "notification:";

/// `DeepLink::source` of a link published from `response`:
/// `notification:<id>`, or `notification:<id>:<action>` for an action.
#[must_use]
pub fn response_source(response: &NotificationResponse) -> String {
    match &response.action {
        ResponseAction::Button(action) => {
            format!(
                "{SOURCE_PREFIX}{}:{}",
                response.id.as_str(),
                action.as_str()
            )
        }
        _ => format!("{SOURCE_PREFIX}{}", response.id.as_str()),
    }
}

/// A flat view of a response for `match`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Response<'a> {
    /// The notification was tapped.
    Open,
    /// An action without text, by id.
    Action(&'a str),
    /// A reply (or a chosen choice), with its text.
    Reply(&'a str),
    /// Dismissed.
    Dismissed,
    /// The simulated "More" action.
    More,
}

impl NotificationResponse {
    /// The response as a flat [`Response`].
    #[must_use]
    pub fn kind(&self) -> Response<'_> {
        match &self.action {
            ResponseAction::Open => Response::Open,
            ResponseAction::Button(action) => self
                .reply
                .as_deref()
                .map_or(Response::Action(action.as_str()), Response::Reply),
            ResponseAction::Dismiss => Response::Dismissed,
            ResponseAction::More => Response::More,
        }
    }

    /// The deep link this response publishes: only a tap on the
    /// notification or a button opens a URL; a dismissal or a "More"
    /// never does.
    #[must_use]
    pub fn deep_link(&self, received_at_ms: Option<u64>) -> Option<DeepLink> {
        let opens = match self.action {
            ResponseAction::Open | ResponseAction::Button(_) => true,
            ResponseAction::Dismiss | ResponseAction::More => false,
        };
        let url = self.url.as_ref().filter(|_| opens)?;
        Some(DeepLink {
            uri: url.clone(),
            source: Some(response_source(self)),
            received_at_ms,
        })
    }
}

/// Reader of the responses lane.
#[derive(Debug, Clone)]
pub struct NotificationResponses {
    queue: Arc<PreMainQueue>,
}

impl Plugin for NotificationResponses {
    const PLUGIN_ID: &'static str = NOTIFICATIONS_PLUGIN_ID;
}

impl NotificationResponses {
    /// Reader bound to the process-wide runtime.
    ///
    /// # Errors
    ///
    /// Fails when no global runtime is installed or the plugin was not
    /// declared.
    pub fn acquire() -> Result<Self, IstmoError> {
        Self::from_runtime(&Runtime::global()?)
    }

    /// Reader bound to `rt`.
    ///
    /// # Errors
    ///
    /// [`IstmoError::PluginNotDeclared`] when enforcement is on and the
    /// plugin was not declared.
    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            queue: rt
                .early_events()
                .queue(RESPONSES_CHANNEL, RESPONSES_CAPACITY),
        })
    }

    /// Subscribe: the first subscriber drains the responses buffered so
    /// far.
    #[must_use]
    pub fn stream(&self) -> NotificationResponseStream {
        NotificationResponseStream {
            rx: self.queue.subscribe(),
            _marker: PhantomData,
        }
    }

    /// Publishes `response` from Rust (desktop hosts, tests), and its
    /// deep link when it has a URL.
    ///
    /// # Errors
    ///
    /// [`IstmoError::Codec`] when the response does not encode.
    pub fn publish(rt: &Runtime, response: &NotificationResponse) -> Result<(), IstmoError> {
        let bytes = codec::encode(response)?;
        rt.publish_early_queue(RESPONSES_CHANNEL, RESPONSES_CAPACITY, bytes);
        let received = crate::builder::now_ms();
        if let Some(link) = response.deep_link(u64::try_from(received).ok()) {
            let bytes = codec::encode(&link)?;
            rt.publish_early_queue(DEEPLINKS_CHANNEL, DEFAULT_DEEPLINKS_CAPACITY, bytes);
        }
        Ok(())
    }
}

/// Responses as they arrive.
#[derive(Debug)]
pub struct NotificationResponseStream {
    rx: flume::Receiver<Vec<u8>>,
    _marker: PhantomData<fn() -> NotificationResponse>,
}

impl NotificationResponseStream {
    /// Block until the next response.
    ///
    /// # Errors
    ///
    /// [`IstmoError::ChannelClosed`] once the lane is gone, or
    /// [`IstmoError::Codec`] when the payload does not decode.
    pub fn recv(&self) -> Result<NotificationResponse, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Async variant of [`recv`](Self::recv).
    ///
    /// # Errors
    ///
    /// Same as [`recv`](Self::recv).
    pub async fn recv_async(&self) -> Result<NotificationResponse, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// The next buffered response without blocking, `None` when nothing
    /// is buffered.
    #[must_use]
    pub fn try_recv(&self) -> Option<Result<NotificationResponse, IstmoError>> {
        self.rx.try_recv().ok().map(|bytes| decode(&bytes))
    }
}

fn decode(bytes: &[u8]) -> Result<NotificationResponse, IstmoError> {
    let (value, _) = codec::decode::<NotificationResponse>(bytes)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionId, NotificationId};

    fn response(action: ResponseAction) -> NotificationResponse {
        NotificationResponse {
            id: NotificationId::try_from("n").unwrap(),
            action,
            reply: None,
            input_requested: false,
            user_data: None,
            url: Some("app://x".to_owned()),
            launched_app: false,
            overflow_actions: Vec::new(),
            ack: None,
        }
    }

    #[test]
    fn only_opening_responses_publish_their_url() {
        let button = ResponseAction::Button(ActionId::try_from("go").unwrap());
        for action in [ResponseAction::Open, button] {
            let link = response(action).deep_link(Some(5)).unwrap();
            assert_eq!(link.uri, "app://x");
            assert_eq!(link.received_at_ms, Some(5));
        }
        for action in [ResponseAction::Dismiss, ResponseAction::More] {
            assert_eq!(response(action).deep_link(None), None);
        }
    }
}
