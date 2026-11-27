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
//! closed. The backend keeps the fire time of every scheduled toast it
//! (or an earlier process) armed ([`Marks`]). When the host delivers
//! such an occurrence itself (the app runs at that time, or a missed
//! delivery at the next start), the pending scheduled toast is removed
//! first; if Windows already showed it (the history holds a toast of
//! that tag marked with that fire time), the host's toast replaces it
//! without a popup. Shows of ids without a due mark skip all of this.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

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
use super::marks::{DELIVERY_SLACK_MS, Marks, SHOWN_WINDOW_MS};
use super::registration::{platform, registered};
use super::toast_xml::{self, Carry, Options, Toast};
use super::{GROUP, activator, ident};
use crate::desktop::{Backend, BackendInfo, BackendSink, ShowMode, Shown, Upcoming};
use crate::{
    DeliveredNotification, DismissReason, GroupId, Interruption, NotificationError, NotificationId,
    PermissionState, PermissionStatus, PlatformError,
};

/// Toasts kept for data-binding updates: twice what the notification
/// center keeps, so a toast it dropped silently goes eventually (the
/// cost of forgetting one too early is a full replace with
/// `SuppressPopup` instead of an update).
const MAX_SHOWN: usize = 2 * super::MAX_ACTIVE as usize;
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
        shown: Arc::default(),
        marks: Mutex::default(),
    });
    // Scheduled toasts of an earlier process may be pending or just shown.
    backend.inherit_marks();
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
    /// Insertion order: the oldest goes past [`MAX_SHOWN`].
    order: u64,
}

impl ShownToast {
    fn unhook(&self) {
        let _dismissed = self.toast.RemoveDismissed(self.dismissed);
        let _failed = self.toast.RemoveFailed(self.failed);
    }
}

/// The toasts this process shows, bounded: dismissed and failed ones
/// leave from their event handlers, the oldest past [`MAX_SHOWN`] on
/// insert.
#[derive(Default)]
struct ShownTable {
    toasts: HashMap<NotificationId, ShownToast>,
    next_order: u64,
}

impl ShownTable {
    /// Puts `toast` under `id`; what it replaced or pushed out (to unhook
    /// outside the lock).
    fn insert(&mut self, id: NotificationId, mut toast: ShownToast) -> Vec<ShownToast> {
        self.next_order += 1;
        toast.order = self.next_order;
        let mut gone: Vec<ShownToast> = self.toasts.insert(id, toast).into_iter().collect();
        while self.toasts.len() > MAX_SHOWN {
            let Some(oldest) = self
                .toasts
                .iter()
                .min_by_key(|(_, entry)| entry.order)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            gone.extend(self.toasts.remove(&oldest));
        }
        gone
    }

    /// Forgets `id` when it is still `toast` (not a newer one), from
    /// inside the toast's own event: no unhooking there, the toast is
    /// done and releases its handlers with itself.
    fn forget(table: &Weak<Mutex<Self>>, id: &NotificationId, toast: Option<&ToastNotification>) {
        let Some(table) = table.upgrade() else {
            return;
        };
        let gone = {
            let mut table = lock(&table);
            match table.toasts.get(id) {
                Some(entry) if Some(&entry.toast) == toast => table.toasts.remove(id),
                _ => None,
            }
        };
        drop(gone);
    }
}

struct ToastBackend {
    aumid: HSTRING,
    group: HSTRING,
    notifier: ToastNotifier,
    build: u32,
    sink: BackendSink,
    shown: Arc<Mutex<ShownTable>>,
    /// Fire times of the scheduled toasts of this app.
    marks: Mutex<Marks>,
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
            match shown.toasts.get(id) {
                Some(entry) if entry.template == toast.template => entry.sequence + 1,
                _ => return false,
            }
        };
        let updated = data_of(toast, sequence)
            .and_then(|data| self.notifier.UpdateWithTagAndGroup(&data, tag, &self.group));
        match updated {
            Ok(NotificationUpdateResult::Succeeded) => {
                if let Some(entry) = lock(&self.shown).toasts.get_mut(id) {
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

    /// The fire time a toast of the history was scheduled for.
    fn fired_at(toast: &ToastNotification) -> Option<i64> {
        toast
            .Content()
            .ok()
            .and_then(|content| launch_args(&content))
            .and_then(|args| args.fire_ms)
    }

    /// Marks what an earlier process scheduled: pending scheduled toasts
    /// and scheduled toasts the history shows as fired within the window.
    fn inherit_marks(&self) {
        let now = Self::now();
        let mut found = Vec::new();
        if let Ok(scheduled) = self.notifier.GetScheduledToastNotifications() {
            for toast in &scheduled {
                if toast.Group().is_ok_and(|g| g == self.group)
                    && let (Ok(tag), Ok(at)) = (toast.Tag(), toast.DeliveryTime())
                {
                    found.push((tag.to_string(), unix_ms(at)));
                }
            }
        }
        if let Ok(history) =
            Self::history().and_then(|h| h.GetHistoryWithId(&self.aumid).map_err(|e| platform(&e)))
        {
            for toast in &history {
                if toast.Group().is_ok_and(|g| g == self.group)
                    && let Some(fire) = Self::fired_at(&toast)
                    && fire >= now.saturating_sub(SHOWN_WINDOW_MS)
                    && let Ok(tag) = toast.Tag()
                {
                    found.push((tag.to_string(), fire));
                }
            }
        }
        let mut marks = lock(&self.marks);
        for (tag, fire) in &found {
            marks.mark(tag, *fire);
        }
    }

    /// Before the host's own alerting show of `id`: when a scheduled toast
    /// stands for this delivery, drops it if still pending and tells
    /// whether Windows already showed it. Ids without a due mark cost no
    /// `WinRT` call.
    fn shown_by_schedule(&self, id: &NotificationId, tag: &HSTRING) -> bool {
        let now = Self::now();
        let Some(fire) = lock(&self.marks).take_due(id.as_str(), now) else {
            return false;
        };
        if let Ok(scheduled) = self.notifier.GetScheduledToastNotifications() {
            let mut pending = false;
            for toast in &scheduled {
                let ours = toast.Group().is_ok_and(|g| g == self.group)
                    && toast.Tag().is_ok_and(|t| t == *tag);
                let Some(at) = toast.DeliveryTime().ok().map(unix_ms) else {
                    continue;
                };
                if ours
                    && at <= now.saturating_add(DELIVERY_SLACK_MS)
                    && self.notifier.RemoveFromSchedule(&toast).is_ok()
                {
                    pending |= at == fire;
                }
            }
            // Taken out before it fired: the host's toast is the only one.
            if pending {
                return false;
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
                && Self::fired_at(&toast) == Some(fire)
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
            .toasts
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
        let table = Arc::downgrade(&self.shown);
        let dismissed = notification.Dismissed(&TypedEventHandler::<
            ToastNotification,
            ToastDismissedEventArgs,
        >::new(move |sender, args| {
            // TimedOut = the popup went to the notification center (still
            // there); ApplicationHidden = removed by this app.
            if let Some(args) = args.as_ref()
                && args.Reason()? == ToastDismissalReason::UserCanceled
            {
                ShownTable::forget(&table, &id, sender.as_ref());
                sink.dismissed(&id, DismissReason::User);
            }
            Ok(())
        }))?;
        let sink = self.sink.clone();
        let id = n.id.clone();
        let table = Arc::downgrade(&self.shown);
        let failed = notification.Failed(&TypedEventHandler::<
            ToastNotification,
            ToastFailedEventArgs,
        >::new(move |sender, args| {
            let code = args
                .as_ref()
                .and_then(|args| args.ErrorCode().ok())
                .map_or(0, |code| code.0);
            ShownTable::forget(&table, &id, sender.as_ref());
            sink.failed(Some(&id), &format!("toast failed (HRESULT {code:#010x})"));
            Ok(())
        }))?;
        self.notifier.Show(&notification)?;
        let gone = lock(&self.shown).insert(
            n.id.clone(),
            ShownToast {
                toast: notification,
                template: toast.template,
                sequence,
                dismissed,
                failed,
                order: 0,
            },
        );
        for entry in &gone {
            entry.unhook();
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
        // An alerting show (new, or a repeat replacing the previous
        // occurrence) may be the delivery a scheduled toast just made.
        let suppress = silent || self.shown_by_schedule(&n.id, &tag);
        self.post(shown, &tag, &toast, suppress)
            .map_err(|err| platform(&err))
    }

    fn remove(&self, id: &NotificationId) -> Result<(), NotificationError> {
        let entry = lock(&self.shown).toasts.remove(id);
        if let Some(entry) = entry {
            entry.unhook();
        }
        Self::history()?
            .RemoveGroupedTagWithId(&HSTRING::from(id.as_str()), &self.group, &self.aumid)
            .map_err(|err| platform(&err))
    }

    fn remove_all(&self) -> Result<(), NotificationError> {
        let entries: Vec<ShownToast> = lock(&self.shown)
            .toasts
            .drain()
            .map(|(_, entry)| entry)
            .collect();
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
        let mut desired: HashMap<String, (&str, i64, String)> = HashMap::new();
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
                    desired.insert(id, (n.id.as_str(), item.fire_ms, toast.xml));
                }
                Err(err) => {
                    tracing::debug!(%err, id = %n.id, "notifications: not scheduled natively");
                }
            }
        }
        // Mark changes, applied at the end (no lock across `WinRT` calls).
        let mut marked: Vec<(String, i64)> = Vec::new();
        let mut unmarked: Vec<(String, i64)> = Vec::new();
        match self.notifier.GetScheduledToastNotifications() {
            Ok(scheduled) => {
                for toast in &scheduled {
                    if !toast.Group().is_ok_and(|g| g == self.group) {
                        continue;
                    }
                    let id = toast.Id().map(|id| id.to_string()).unwrap_or_default();
                    let at = toast.DeliveryTime().ok().map(unix_ms);
                    let tag = toast.Tag().map(|tag| tag.to_string()).ok();
                    if desired.remove(&id).is_some() {
                        if let (Some(tag), Some(at)) = (tag, at) {
                            marked.push((tag, at));
                        }
                    } else if let Err(err) = self.notifier.RemoveFromSchedule(&toast) {
                        tracing::debug!(%err, "notifications: scheduled toast not removed");
                    } else if let (Some(tag), Some(at)) = (tag, at)
                        && at > now.saturating_add(DELIVERY_SLACK_MS)
                    {
                        // It cannot fire any more. One due now may have
                        // fired already: its mark stays for the history
                        // check.
                        unmarked.push((tag, at));
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
                    toast.SetTag(&HSTRING::from(tag))?;
                    toast.SetGroup(&self.group)?;
                    self.notifier.AddToSchedule(&toast)
                });
            match added {
                Ok(()) => marked.push((tag.to_owned(), fire_ms)),
                Err(err) => tracing::warn!(%err, "notifications: scheduled toast not added"),
            }
        }
        let mut marks = lock(&self.marks);
        for (tag, at) in &unmarked {
            marks.unmark(tag, *at);
        }
        for (tag, at) in &marked {
            marks.mark(tag, *at);
        }
        marks.prune(now);
    }
}
