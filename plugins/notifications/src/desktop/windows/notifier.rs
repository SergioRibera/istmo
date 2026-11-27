//! The [`Backend`] over `ToastNotifier`: show, replace in place,
//! data-binding updates, removal through the history, the delivered
//! list, scheduled toasts, and the Dismissed / Failed events.
//!
//! Every toast has tag = notification id and group = [`GROUP`]. A
//! silent update whose XML only differs in bound values is a
//! `ToastNotifier.Update` (no popup, stays in place); anything else is
//! a full replace, with `SuppressPopup` when silent.
//!
//! Scheduled toasts ([`Backend::arm`]) cover deliveries while the app is
//! closed. When the host delivers the same occurrence itself (the app
//! runs at that time, or a missed delivery at the next start), the
//! pending scheduled toast is removed first; if Windows already showed
//! it (the history holds a toast of that tag marked with a fire time
//! within the last day), the host's toast replaces it without a popup.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use istmo_core::sync::lock;
use windows::Data::Xml::Dom::{XmlDocument, XmlElement};
use windows::Foundation::{DateTime, IReference, PropertyValue, TypedEventHandler};
use windows::UI::Notifications::{
    NotificationData, NotificationSetting, NotificationUpdateResult, ScheduledToastNotification,
    ToastDismissalReason, ToastDismissedEventArgs, ToastFailedEventArgs, ToastNotification,
    ToastNotificationHistory, ToastNotificationManager, ToastNotificationPriority, ToastNotifier,
};
use windows_core::{HSTRING, Interface};

use super::args::Args;
use super::registration::{platform, registered};
use super::toast_xml::{self, Carry, Options, Toast};
use super::{GROUP, activator, ident};
use crate::desktop::{Backend, BackendInfo, BackendSink, ShowMode, Shown, Upcoming};
use crate::{
    DeliveredNotification, DismissReason, GroupId, Interruption, NotificationError, NotificationId,
    PermissionState, PermissionStatus, PlatformError,
};

/// A scheduled delivery this close to now is the host's own delivery.
const DELIVERY_SLACK_MS: i64 = 60_000;
/// How long a toast shown by a scheduled toast counts as "just shown".
const SCHEDULED_SHOWN_WINDOW_MS: i64 = 24 * 3_600_000;
/// Windows `DateTime` ticks (100 ns) between 1601-01-01 and 1970-01-01.
const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;

/// Starts the backend over the registration of
/// [`DesktopNotifications::install`](crate::DesktopNotifications::install).
pub(crate) fn start(sink: BackendSink) -> Result<Arc<dyn Backend>, NotificationError> {
    let registered = registered().ok_or_else(|| {
        NotificationError::Platform(PlatformError {
            code: 0,
            message: "Windows: DesktopNotifications::install was not called (or failed)".to_owned(),
        })
    })?;
    let aumid = HSTRING::from(registered.aumid.as_str());
    let notifier =
        ToastNotificationManager::CreateToastNotifierWithId(&aumid).map_err(|e| platform(&e))?;
    let backend = Arc::new(ToastBackend {
        aumid,
        group: HSTRING::from(GROUP),
        notifier,
        build: registered.build,
        sink: sink.clone(),
        shown: Mutex::new(HashMap::new()),
        // Until the first arm: a scheduled toast of an earlier process may
        // be pending or just shown.
        armed: AtomicBool::new(true),
    });
    activator::attach(sink);
    Ok(backend)
}

/// A toast this process put up.
struct ShownToast {
    toast: ToastNotification,
    template: u64,
    sequence: u32,
    dismissed: i64,
    failed: i64,
}

impl ShownToast {
    fn unhook(&self) {
        let _dismissed = self.toast.RemoveDismissed(self.dismissed);
        let _failed = self.toast.RemoveFailed(self.failed);
    }
}

struct ToastBackend {
    aumid: HSTRING,
    group: HSTRING,
    notifier: ToastNotifier,
    build: u32,
    sink: BackendSink,
    shown: Mutex<HashMap<NotificationId, ShownToast>>,
    /// Scheduled toasts of this app may exist.
    armed: AtomicBool,
}

const fn date_time(unix_ms: i64) -> DateTime {
    DateTime {
        UniversalTime: unix_ms
            .saturating_mul(10_000)
            .saturating_add(UNIX_EPOCH_TICKS),
    }
}

const fn unix_ms(time: DateTime) -> i64 {
    time.UniversalTime.saturating_sub(UNIX_EPOCH_TICKS) / 10_000
}

fn data_of(toast: &Toast, sequence: u32) -> windows_core::Result<NotificationData> {
    let data = NotificationData::new()?;
    let values = data.Values()?;
    for (key, value) in &toast.values {
        values.Insert(&HSTRING::from(*key), &HSTRING::from(value.as_str()))?;
    }
    data.SetSequenceNumber(sequence)?;
    Ok(data)
}

fn document(xml: &str) -> windows_core::Result<XmlDocument> {
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    Ok(doc)
}

/// The `launch` arguments of a toast's content.
fn launch_args(content: &XmlDocument) -> Option<Args> {
    let launch = content
        .DocumentElement()
        .and_then(|root| root.GetAttribute(&HSTRING::from("launch")))
        .ok()?;
    Args::decode(&launch.to_string())
}

/// Attribute `name` of the first `element` of `doc`.
fn first_attribute(doc: &XmlDocument, element: &str, name: &str) -> Option<String> {
    let nodes = doc.GetElementsByTagName(&HSTRING::from(element)).ok()?;
    let node = nodes.Item(0).ok()?;
    let value = node
        .cast::<XmlElement>()
        .and_then(|e| e.GetAttribute(&HSTRING::from(name)))
        .ok()?;
    (!value.is_empty()).then(|| value.to_string())
}

/// The texts of the `text` elements of `doc`, in order.
fn texts(doc: &XmlDocument) -> Vec<String> {
    let Ok(nodes) = doc.GetElementsByTagName(&HSTRING::from("text")) else {
        return Vec::new();
    };
    (&nodes)
        .into_iter()
        .filter_map(|node| node.InnerText().ok())
        .map(|text| text.to_string())
        .collect()
}

impl ToastBackend {
    fn history() -> Result<ToastNotificationHistory, NotificationError> {
        ToastNotificationManager::History().map_err(|e| platform(&e))
    }

    fn now() -> i64 {
        crate::builder::now_ms()
    }

    /// A data-binding update when the template is unchanged; `false`
    /// when a full show is needed.
    fn try_update(&self, id: &NotificationId, tag: &HSTRING, toast: &Toast) -> bool {
        let sequence = {
            let shown = lock(&self.shown);
            match shown.get(id) {
                Some(entry) if entry.template == toast.template => entry.sequence + 1,
                _ => return false,
            }
        };
        let updated = data_of(toast, sequence)
            .and_then(|data| self.notifier.UpdateWithTagAndGroup(&data, tag, &self.group));
        match updated {
            Ok(NotificationUpdateResult::Succeeded) => {
                if let Some(entry) = lock(&self.shown).get_mut(id) {
                    entry.sequence = entry.sequence.max(sequence);
                }
                true
            }
            // Gone from the notification center (dismissed): replace.
            Ok(_) => false,
            Err(err) => {
                tracing::debug!(%err, "notifications: toast update failed; replacing");
                false
            }
        }
    }

    /// Before the host's own delivery of `tag`: drops a scheduled toast
    /// due now and tells whether Windows already showed one.
    fn shown_by_schedule(&self, tag: &HSTRING) -> bool {
        if !self.armed.load(Ordering::Acquire) {
            return false;
        }
        let now = Self::now();
        if let Ok(scheduled) = self.notifier.GetScheduledToastNotifications() {
            for toast in &scheduled {
                let ours = toast.Group().is_ok_and(|g| g == self.group)
                    && toast.Tag().is_ok_and(|t| t == *tag);
                let due = toast
                    .DeliveryTime()
                    .is_ok_and(|at| unix_ms(at) <= now + DELIVERY_SLACK_MS);
                if ours && due {
                    let _removed = self.notifier.RemoveFromSchedule(&toast);
                }
            }
        }
        let Ok(history) =
            Self::history().and_then(|h| h.GetHistoryWithId(&self.aumid).map_err(|e| platform(&e)))
        else {
            return false;
        };
        (&history).into_iter().any(|toast| {
            toast.Group().is_ok_and(|g| g == self.group)
                && toast.Tag().is_ok_and(|t| t == *tag)
                && toast
                    .Content()
                    .ok()
                    .and_then(|content| launch_args(&content))
                    .and_then(|args| args.fire_ms)
                    .is_some_and(|fire| {
                        fire >= now - SCHEDULED_SHOWN_WINDOW_MS && fire <= now + DELIVERY_SLACK_MS
                    })
        })
    }

    fn post(
        &self,
        shown: &Shown<'_>,
        tag: &HSTRING,
        toast: &Toast,
        suppress_popup: bool,
    ) -> windows_core::Result<()> {
        let n = shown.notification;
        let sequence = lock(&self.shown)
            .get(&n.id)
            .map_or(1, |entry| entry.sequence + 1);
        let notification = ToastNotification::CreateToastNotification(&document(&toast.xml)?)?;
        notification.SetTag(tag)?;
        notification.SetGroup(&self.group)?;
        notification.SetData(&data_of(toast, sequence)?)?;
        if suppress_popup {
            notification.SetSuppressPopup(true)?;
        }
        if n.interruption >= Some(Interruption::High) {
            notification.SetPriority(ToastNotificationPriority::High)?;
        }
        if let Some(ms) = n.timeout_ms {
            let at = Self::now().saturating_add(i64::try_from(ms).unwrap_or(i64::MAX));
            let at: IReference<DateTime> = PropertyValue::CreateDateTime(date_time(at))?.cast()?;
            notification.SetExpirationTime(&at)?;
        }
        let sink = self.sink.clone();
        let id = n.id.clone();
        let dismissed = notification.Dismissed(&TypedEventHandler::<
            ToastNotification,
            ToastDismissedEventArgs,
        >::new(move |_, args| {
            // TimedOut = the popup went to the notification center (still
            // there); ApplicationHidden = removed by this app.
            if let Some(args) = args.as_ref()
                && args.Reason()? == ToastDismissalReason::UserCanceled
            {
                sink.dismissed(&id, DismissReason::User);
            }
            Ok(())
        }))?;
        let sink = self.sink.clone();
        let id = n.id.clone();
        let failed = notification.Failed(&TypedEventHandler::<
            ToastNotification,
            ToastFailedEventArgs,
        >::new(move |_, args| {
            let code = args
                .as_ref()
                .and_then(|args| args.ErrorCode().ok())
                .map_or(0, |code| code.0);
            sink.failed(Some(&id), &format!("toast failed (HRESULT {code:#010x})"));
            Ok(())
        }))?;
        self.notifier.Show(&notification)?;
        let previous = lock(&self.shown).insert(
            n.id.clone(),
            ShownToast {
                toast: notification,
                template: toast.template,
                sequence,
                dismissed,
                failed,
            },
        );
        if let Some(previous) = previous {
            previous.unhook();
        }
        Ok(())
    }
}

impl Backend for ToastBackend {
    fn info(&self) -> BackendInfo {
        super::info(self.build, true)
    }

    fn permission(&self) -> PermissionStatus {
        match self.notifier.Setting() {
            Ok(NotificationSetting::Enabled) => PermissionStatus::of(PermissionState::Granted),
            Ok(_) => PermissionStatus::of(PermissionState::Denied),
            Err(err) => {
                tracing::debug!(%err, "notifications: toast setting unreadable");
                PermissionStatus::of(PermissionState::Unavailable)
            }
        }
    }

    fn open_settings(&self, _channel: Option<&crate::ChannelId>) -> Result<(), NotificationError> {
        std::process::Command::new("explorer.exe")
            .arg("ms-settings:notifications")
            .spawn()
            .map(drop)
            .map_err(|err| {
                NotificationError::Platform(PlatformError {
                    code: 0,
                    message: format!("settings not opened: {err}"),
                })
            })
    }

    fn show(&self, shown: &Shown<'_>) -> Result<(), NotificationError> {
        let n = shown.notification;
        let tag = HSTRING::from(n.id.as_str());
        let toast = toast_xml::build_fitting(
            n,
            shown.rendered,
            shown.images,
            Options {
                build: self.build,
                bind: true,
                fire_ms: None,
                carry: Carry::All,
            },
        )?;
        let silent = shown.mode == ShowMode::Replace { alert: false };
        if silent && self.try_update(&n.id, &tag, &toast) {
            return Ok(());
        }
        let suppress = silent || (shown.mode == ShowMode::New && self.shown_by_schedule(&tag));
        self.post(shown, &tag, &toast, suppress)
            .map_err(|err| platform(&err))
    }

    fn remove(&self, id: &NotificationId) -> Result<(), NotificationError> {
        let entry = lock(&self.shown).remove(id);
        if let Some(entry) = entry {
            entry.unhook();
        }
        Self::history()?
            .RemoveGroupedTagWithId(&HSTRING::from(id.as_str()), &self.group, &self.aumid)
            .map_err(|err| platform(&err))
    }

    fn remove_all(&self) -> Result<(), NotificationError> {
        let entries: Vec<ShownToast> = lock(&self.shown).drain().map(|(_, entry)| entry).collect();
        for entry in &entries {
            entry.unhook();
        }
        // Also the toasts of earlier processes: every toast of this app is
        // in the group.
        Self::history()?
            .RemoveGroupWithId(&self.group, &self.aumid)
            .map_err(|err| platform(&err))
    }

    fn delivered(&self) -> Result<Vec<DeliveredNotification>, NotificationError> {
        let history = Self::history()?
            .GetHistoryWithId(&self.aumid)
            .map_err(|err| platform(&err))?;
        let mut out = Vec::new();
        for toast in &history {
            if !toast.Group().is_ok_and(|g| g == self.group) {
                continue;
            }
            let Some(id) = toast
                .Tag()
                .ok()
                .and_then(|tag| NotificationId::try_from(tag.to_string()).ok())
            else {
                continue;
            };
            let value = |key: &str| {
                toast
                    .Data()
                    .and_then(|data| data.Values())
                    .and_then(|values| values.Lookup(&HSTRING::from(key)))
                    .ok()
                    .map(|v| v.to_string())
            };
            let content = toast.Content().ok();
            // Shown by a scheduled toast: no data, values written in place.
            let (title, body) = value("title").map_or_else(
                || {
                    let mut texts = content.as_ref().map(texts).unwrap_or_default().into_iter();
                    (texts.next(), texts.next_back())
                },
                |title| (Some(title), value("body")),
            );
            let group = content
                .as_ref()
                .and_then(|doc| first_attribute(doc, "header", "id"))
                .and_then(|id| GroupId::try_from(id).ok());
            out.push(DeliveredNotification {
                id,
                title,
                body,
                group,
                delivered_ms: None,
            });
        }
        Ok(out)
    }

    fn arm(&self, upcoming: &[Upcoming]) {
        let now = Self::now();
        let mut desired: HashMap<String, (HSTRING, i64, String)> = HashMap::new();
        for item in upcoming {
            if item.fire_ms <= now + 2_000 {
                continue;
            }
            let n = &item.notification;
            let toast = toast_xml::build_fitting(
                n,
                &item.rendered,
                &toast_xml::file_images(n),
                Options {
                    build: self.build,
                    bind: false,
                    fire_ms: Some(item.fire_ms),
                    carry: Carry::All,
                },
            );
            match toast {
                Ok(toast) => {
                    let id = ident::scheduled_id(n.id.as_str(), item.fire_ms, &toast.xml);
                    desired.insert(id, (HSTRING::from(n.id.as_str()), item.fire_ms, toast.xml));
                }
                Err(err) => {
                    tracing::debug!(%err, id = %n.id, "notifications: not scheduled natively");
                }
            }
        }
        let mut any = !desired.is_empty();
        match self.notifier.GetScheduledToastNotifications() {
            Ok(scheduled) => {
                for toast in &scheduled {
                    if !toast.Group().is_ok_and(|g| g == self.group) {
                        continue;
                    }
                    let id = toast.Id().map(|id| id.to_string()).unwrap_or_default();
                    if desired.remove(&id).is_some() {
                        any = true;
                    } else if let Err(err) = self.notifier.RemoveFromSchedule(&toast) {
                        tracing::debug!(%err, "notifications: scheduled toast not removed");
                    }
                }
            }
            Err(err) => tracing::debug!(%err, "notifications: scheduled toasts unreadable"),
        }
        for (id, (tag, fire_ms, xml)) in desired {
            let added = document(&xml)
                .and_then(|doc| {
                    ScheduledToastNotification::CreateScheduledToastNotification(
                        &doc,
                        date_time(fire_ms),
                    )
                })
                .and_then(|toast| {
                    toast.SetId(&HSTRING::from(id.as_str()))?;
                    toast.SetTag(&tag)?;
                    toast.SetGroup(&self.group)?;
                    self.notifier.AddToSchedule(&toast)
                });
            if let Err(err) = added {
                tracing::warn!(%err, "notifications: scheduled toast not added");
            }
        }
        self.armed.store(any, Ordering::Release);
    }
}
