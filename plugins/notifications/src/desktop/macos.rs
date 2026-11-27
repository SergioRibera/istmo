//! macOS: `UNUserNotificationCenter` through objc2.
//!
//! Only a bundled app (`.app` with a bundle id) may use it: anywhere
//! else the start fails with [`NotificationError::NotBundled`] (logged
//! once) and the host answers like `UnavailableNotifications`. Rules that
//! do not touch the framework live in [`super::apple_map`].
//!
//! Threads: every `UNUserNotificationCenter` method used here is
//! thread-safe; backend methods run on the host's blocking workers and
//! wait (bounded) for the framework's completion handlers, which run on
//! its private queue, never on the main thread. Nothing here waits for
//! the main thread.
//!
//! Responses: the delegate (set when the backend starts; the app attaches
//! the host before its event loop, so before launch finishes) hands every
//! tap, button, reply and dismissal to the [`BackendSink`] and calls the
//! completion handler at once: macOS does not suspend the process, so a
//! background action needs no held handler and `complete(ack)` releases
//! nothing.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use block2::RcBlock;
use istmo_core::sync::lock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_app_kit::{
    NSApplicationDidFinishLaunchingNotification, NSApplicationLaunchIsDefaultLaunchKey,
    NSApplicationLaunchUserNotificationKey, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSBundle, NSData, NSDictionary, NSError, NSNotification, NSNotificationCenter,
    NSNumber, NSObject, NSObjectProtocol, NSOperatingSystemVersion, NSProcessInfo, NSSet, NSString,
    NSURL,
};
use objc2_intents::{
    INImage, INInteraction, INInteractionDirection, INOutgoingMessageType, INPerson,
    INPersonHandle, INPersonHandleType, INSendMessageIntent,
};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationAttachment, UNNotificationCategory,
    UNNotificationCategoryOptions, UNNotificationContent, UNNotificationInterruptionLevel,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSettings, UNNotificationSound, UNNotificationSoundName,
    UNTextInputNotificationAction, UNTextInputNotificationResponse, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use super::apple_map::{self, RawSettings, ResponseKind, SoundChoice, keys};
use super::backend::{Activation, Backend, BackendInfo, BackendSink, Carried, ImageSlot, Shown};
use crate::render::{self, Input, RenderLimits};
use crate::{
    ChannelId, DeliveredNotification, DismissReason, GroupId, Notification, NotificationError,
    NotificationId, PermissionRequest, PermissionStatus, PlatformError, Style,
};

/// Longest wait for a completion handler.
const WAIT: Duration = Duration::from_secs(5);
/// Categories this backend keeps registered (oldest dropped first).
const MAX_CATEGORIES: usize = 128;
/// Longest wait for the user to answer the permission prompt.
const PROMPT_WAIT: Duration = Duration::from_secs(300);

/// Logged once: the app is not bundled.
static NOT_BUNDLED_LOGGED: AtomicBool = AtomicBool::new(false);
/// The launch was a notification activation; the first response takes it.
static LAUNCHED_BY_NOTIFICATION: AtomicBool = AtomicBool::new(false);
/// Attachment copies, numbered per process.
static ATTACHMENTS: AtomicU64 = AtomicU64::new(0);

/// Starts the macOS backend.
pub(super) fn start(sink: BackendSink) -> Result<Arc<dyn Backend>, NotificationError> {
    let bundle = NSBundle::mainBundle();
    let bundle_id = bundle.bundleIdentifier().map(|id| id.to_string());
    if !apple_map::is_bundled(&bundle.bundlePath().to_string(), bundle_id.as_deref()) {
        if !NOT_BUNDLED_LOGGED.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "notifications: macOS needs a bundled .app with a bundle id; notifications are off"
            );
        }
        return Err(NotificationError::NotBundled);
    }
    let bundle_id = bundle_id.unwrap_or_default();
    let os = NSProcessInfo::processInfo().operatingSystemVersion();
    let monterey = at_least(12, 0);
    let communication = monterey && lists_send_message(&bundle);

    observe_launch();
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let delegate = Delegate::new(sink);
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // The delegate property is weak: the delegate lives for the process.
    std::mem::forget(delegate);

    Ok(Arc::new(Mac {
        os_version: format!("{}.{}", os.majorVersion, os.minorVersion),
        bundle_id,
        communication,
        monterey,
        ventura: at_least(13, 0),
        categories: Mutex::new(Categories::default()),
        attachments: std::env::temp_dir().join("istmo-notifications"),
    }))
}

fn at_least(major: isize, minor: isize) -> bool {
    NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
        majorVersion: major,
        minorVersion: minor,
        patchVersion: 0,
    })
}

/// `INSendMessageIntent` in `NSUserActivityTypes` (written by istmo-build
/// for `[app.notifications] apple.communication = true`).
fn lists_send_message(bundle: &NSBundle) -> bool {
    let Some(types) = bundle.objectForInfoDictionaryKey(&NSString::from_str("NSUserActivityTypes"))
    else {
        return false;
    };
    let Ok(types) = types.downcast::<NSArray>() else {
        return false;
    };
    types.iter().any(|entry| {
        entry
            .downcast_ref::<NSString>()
            .is_some_and(|name| name.to_string() == "INSendMessageIntent")
    })
}

/// Watches `NSApplicationDidFinishLaunchingNotification` for a launch
/// that a notification caused. Registered once; misses the launch when
/// the backend starts after it (then no response reports
/// `launched_app`).
fn observe_launch() {
    static OBSERVING: AtomicBool = AtomicBool::new(false);
    if OBSERVING.swap(true, Ordering::AcqRel) {
        return;
    }
    let block = RcBlock::new(|note: NonNull<NSNotification>| {
        // SAFETY: the notification center passes a valid notification
        // for the duration of the block.
        let note = unsafe { note.as_ref() };
        let Some(info) = note.userInfo() else {
            return;
        };
        // SAFETY: AppKit's constant, initialised before any notification.
        let user_key: &NSString = unsafe { NSApplicationLaunchUserNotificationKey };
        // SAFETY: as above.
        let default_key: &NSString = unsafe { NSApplicationLaunchIsDefaultLaunchKey };
        let by_notification = info.objectForKey(user_key.as_ref()).is_some();
        let is_default = info
            .objectForKey(default_key.as_ref())
            .and_then(|value| value.downcast::<NSNumber>().ok())
            .map(|value| value.as_bool());
        if apple_map::launched_by_notification(by_notification, is_default) {
            LAUNCHED_BY_NOTIFICATION.store(true, Ordering::Release);
        }
    });
    // SAFETY: AppKit's constant, initialised before any notification.
    let name = unsafe { NSApplicationDidFinishLaunchingNotification };
    // SAFETY: the block takes the `NSNotification` the method passes; no
    // object filter, no queue (runs on the posting thread).
    let token = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(name),
            None,
            None,
            &block,
        )
    };
    // Observers stay for the process.
    std::mem::forget(token);
}

/// The macOS backend.
struct Mac {
    os_version: String,
    bundle_id: String,
    /// Communication notifications are set up (macOS 12+).
    communication: bool,
    /// macOS 12+: interruption levels, relevance, time-sensitive setting.
    monterey: bool,
    /// macOS 13+: focus filter criteria.
    ventura: bool,
    categories: Mutex<Categories>,
    /// Attachment copies (the framework moves attachment files away).
    attachments: PathBuf,
}

impl std::fmt::Debug for Mac {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mac")
            .field("os_version", &self.os_version)
            .field("communication", &self.communication)
            .finish_non_exhaustive()
    }
}

/// The registered category set (`setNotificationCategories` replaces
/// all of them, the app's own included, so the whole set is kept).
#[derive(Default)]
struct Categories {
    /// Existing categories were read.
    seeded: bool,
    known: HashMap<String, Retained<UNNotificationCategory>>,
    /// This backend's, oldest first.
    ours: VecDeque<String>,
}

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "the retained categories are immutable and thread-safe; see the SAFETY note"
)]
// SAFETY: `UNNotificationCategory` is immutable once created and Apple
// documents it as safe to share between threads; the map is only
// touched under the backend's mutex.
unsafe impl Send for Categories {}

fn ns(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

/// A completion handler's result, sent from the framework's queue.
fn waiter<T: Send + 'static>() -> (flume::Sender<T>, impl FnOnce() -> Option<T>) {
    waiter_for(WAIT)
}

/// [`waiter`] with its own deadline.
fn waiter_for<T: Send + 'static>(
    deadline: Duration,
) -> (flume::Sender<T>, impl FnOnce() -> Option<T>) {
    let (tx, rx) = flume::bounded(1);
    (tx, move || rx.recv_timeout(deadline).ok())
}

fn timed_out(what: &str) -> NotificationError {
    NotificationError::Platform(PlatformError {
        code: -1,
        message: format!("{what}: no answer from UserNotifications"),
    })
}

/// `Some(message)` for an error pointer.
fn error_message(error: *mut NSError) -> Option<(i64, String)> {
    // SAFETY: the framework passes null or a valid `NSError` for the
    // duration of the completion handler.
    let error = unsafe { error.as_ref() }?;
    Some((
        i64::try_from(error.code()).unwrap_or(i64::MIN),
        error.localizedDescription().to_string(),
    ))
}

impl Mac {
    fn center() -> Retained<UNUserNotificationCenter> {
        UNUserNotificationCenter::currentNotificationCenter()
    }

    fn limits(&self, n: &Notification) -> RenderLimits {
        let mut limits = self.info().limits;
        limits.messaging = self.communication && apple_map::communication_sender(n).is_some();
        limits
    }

    /// Reads the categories already registered (the app's and earlier
    /// processes'), once.
    fn seed(categories: &mut Categories) {
        if categories.seeded {
            return;
        }
        categories.seeded = true;
        let (tx, wait) = waiter::<()>();
        let found: Arc<Mutex<Vec<Retained<UNNotificationCategory>>>> = Arc::default();
        let sink = Arc::clone(&found);
        let block = RcBlock::new(move |set: NonNull<NSSet<UNNotificationCategory>>| {
            // SAFETY: the framework passes a valid set for the duration of
            // the completion handler.
            let set = unsafe { set.as_ref() };
            lock(&sink).extend(set.iter());
            let _sent = tx.try_send(());
        });
        Self::center().getNotificationCategoriesWithCompletionHandler(&block);
        if wait().is_none() {
            tracing::warn!("notifications: existing categories not read; they may be replaced");
        }
        for category in lock(&found).drain(..) {
            let id = category.identifier().to_string();
            if id.starts_with("istmo.") {
                categories.ours.push_back(id.clone());
            }
            categories.known.insert(id, category);
        }
    }

    /// Registers the category of `shown` and returns its identifier.
    fn category(&self, shown: &Shown<'_>, communication: bool) -> String {
        let n = shown.notification;
        let id = apple_map::category_id(shown.rendered, n.lock_screen, communication);
        let mut categories = lock(&self.categories);
        Self::seed(&mut categories);
        if categories.known.contains_key(&id) {
            return id;
        }
        let actions: Vec<Retained<UNNotificationAction>> = shown
            .rendered
            .actions
            .iter()
            .map(|action| {
                let options = UNNotificationActionOptions(apple_map::action_options(action));
                let key = ns(&action.key);
                let title = ns(&action.title);
                if action.input == Input::Reply {
                    let (placeholder, submit) = apple_map::reply_texts(n, action);
                    UNTextInputNotificationAction::actionWithIdentifier_title_options_textInputButtonTitle_textInputPlaceholder(
                        &key,
                        &title,
                        options,
                        &ns(&submit),
                        &ns(&placeholder),
                    )
                    .into_super()
                } else {
                    UNNotificationAction::actionWithIdentifier_title_options(&key, &title, options)
                }
            })
            .collect();
        let actions = NSArray::from_retained_slice(&actions);
        let intents: Vec<Retained<NSString>> = if communication {
            vec![ns("INSendMessageIntent")]
        } else {
            Vec::new()
        };
        let intents = NSArray::from_retained_slice(&intents);
        let (options, placeholder) = apple_map::category_options(n.lock_screen);
        let options = UNNotificationCategoryOptions(options);
        let category = placeholder.map_or_else(
            || UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                &ns(&id),
                &actions,
                &intents,
                options,
            ),
            |text| UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_hiddenPreviewsBodyPlaceholder_options(
                &ns(&id),
                &actions,
                &intents,
                &ns(text),
                options,
            ),
        );
        categories.known.insert(id.clone(), category);
        categories.ours.push_back(id.clone());
        while categories.ours.len() > MAX_CATEGORIES {
            if let Some(old) = categories.ours.pop_front() {
                categories.known.remove(&old);
            }
        }
        let all: Vec<Retained<UNNotificationCategory>> =
            categories.known.values().cloned().collect();
        Self::center().setNotificationCategories(&NSSet::from_retained_slice(&all));
        id
    }

    /// A fresh copy of the attached image: the framework moves the file
    /// into its own store. Hard link first (same volume), else a copy.
    fn attachment(&self, source: &Path) -> Option<Retained<UNNotificationAttachment>> {
        if let Err(err) = std::fs::create_dir_all(&self.attachments) {
            tracing::debug!(%err, "notifications: attachment directory not created");
            return None;
        }
        let n = ATTACHMENTS.fetch_add(1, Ordering::Relaxed);
        let ext = source
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("png");
        let copy = self
            .attachments
            .join(format!("{}-{n}.{ext}", std::process::id()));
        if std::fs::hard_link(source, &copy).is_err()
            && let Err(err) = std::fs::copy(source, &copy)
        {
            tracing::debug!(%err, "notifications: attachment not copied");
            return None;
        }
        let url = NSURL::fileURLWithPath(&ns(&copy.to_string_lossy()));
        // SAFETY: no options dictionary is passed.
        let attachment = unsafe {
            UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(
                &ns("istmo.image"),
                &url,
                None,
            )
        };
        match attachment {
            Ok(attachment) => Some(attachment),
            Err(err) => {
                tracing::debug!(error = %err.localizedDescription(), "notifications: attachment refused");
                let _gone = std::fs::remove_file(&copy);
                None
            }
        }
    }

    fn sound(choice: SoundChoice) -> Option<Retained<UNNotificationSound>> {
        match choice {
            SoundChoice::None => None,
            SoundChoice::Default => Some(UNNotificationSound::defaultSound()),
            SoundChoice::Critical => Some(UNNotificationSound::defaultCriticalSound()),
            SoundChoice::Named(name) => {
                let bundle = NSBundle::mainBundle();
                let file = apple_map::SOUND_EXTENSIONS
                    .iter()
                    .find(|ext| {
                        bundle
                            .URLForResource_withExtension(Some(&ns(&name)), Some(&ns(ext)))
                            .is_some()
                    })
                    .map_or_else(|| name.clone(), |ext| format!("{name}.{ext}"));
                let file: Retained<UNNotificationSoundName> = ns(&file);
                Some(UNNotificationSound::soundNamed(&file))
            }
        }
    }

    fn user_info(n: &Notification, quiet: bool) -> Retained<NSDictionary> {
        let mut keys_out: Vec<Retained<NSString>> = vec![ns(keys::ID), ns(keys::PRESENT)];
        let mut values: Vec<Retained<AnyObject>> = vec![
            ns(n.id.as_str()).into(),
            NSNumber::new_u64(apple_map::presentation(n, quiet)).into(),
        ];
        if let Some(data) = &n.user_data {
            keys_out.push(ns(keys::USER_DATA));
            values.push(ns(data).into());
        }
        if let Some(url) = &n.url {
            keys_out.push(ns(keys::URL));
            values.push(ns(url).into());
        }
        let key_refs: Vec<&NSString> = keys_out.iter().map(|key| &**key).collect();
        let dict = NSDictionary::<NSString, AnyObject>::from_retained_objects(&key_refs, &values);
        // SAFETY: the generic parameters are compile-time only; the
        // dictionary's keys are `NSString`s, which are `AnyObject`s.
        unsafe { Retained::cast_unchecked::<NSDictionary>(dict) }
    }

    fn content(&self, shown: &Shown<'_>, quiet: bool) -> Retained<UNNotificationContent> {
        let n = shown.notification;
        let limits = self.limits(n);
        let content = UNMutableNotificationContent::new();
        content.setTitle(&ns(&n.title));
        if let Some(subtitle) = &n.subtitle {
            content.setSubtitle(&ns(subtitle));
        }
        content.setBody(&ns(shown.rendered.body.as_deref().unwrap_or("")));
        if let Some(thread) = apple_map::thread_id(n) {
            content.setThreadIdentifier(&ns(thread));
        }
        content.setCategoryIdentifier(&ns(&self.category(shown, limits.messaging)));
        content.setSound(Self::sound(apple_map::sound(n, quiet)).as_deref());
        if self.monterey {
            content.setInterruptionLevel(UNNotificationInterruptionLevel(
                apple_map::interruption_level(n.interruption, quiet),
            ));
            if let Some(score) = n.apple.as_ref().and_then(|apple| apple.relevance_score) {
                content.setRelevanceScore(f64::from(score.clamp(0.0, 1.0)));
            }
        }
        if self.ventura
            && let Some(criteria) = n
                .apple
                .as_ref()
                .and_then(|apple| apple.filter_criteria.as_deref())
        {
            content.setFilterCriteria(Some(&ns(criteria)));
        }
        if let Some(target) = n
            .apple
            .as_ref()
            .and_then(|apple| apple.target_content_id.as_deref())
        {
            content.setTargetContentIdentifier(Some(&ns(target)));
        }
        if let Some(attachment) =
            apple_map::attachment(shown.images).and_then(|path| self.attachment(path))
        {
            content.setAttachments(&NSArray::from_retained_slice(&[attachment]));
        }
        let info = Self::user_info(n, quiet);
        // SAFETY: every value is a property-list object (strings, numbers).
        unsafe { content.setUserInfo(&info) };
        // Badge untouched: the app badge belongs to istmo-launcher-actions.
        if limits.messaging
            && let Style::Messaging(conversation) = &n.style
        {
            if let Some(updated) = communication(&content, shown, conversation) {
                return updated;
            }
            // Refused: the conversation as body lines.
            let plain = RenderLimits {
                messaging: false,
                ..limits
            };
            content.setBody(&ns(render::body(n, &plain).as_deref().unwrap_or("")));
        }
        content.into_super()
    }

    fn ids(ids: &[&str]) -> Retained<NSArray<NSString>> {
        let ids: Vec<Retained<NSString>> = ids.iter().map(|id| ns(id)).collect();
        NSArray::from_retained_slice(&ids)
    }

    fn settings(&self) -> Result<RawSettings, NotificationError> {
        let (tx, wait) = waiter::<RawSettings>();
        let monterey = self.monterey;
        let block = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // SAFETY: the framework passes valid settings for the duration
            // of the completion handler.
            let s = unsafe { settings.as_ref() };
            let raw = RawSettings {
                authorization: s.authorizationStatus().0,
                alert: s.alertSetting().0,
                sound: s.soundSetting().0,
                badge: s.badgeSetting().0,
                // Selectors of macOS 12.
                time_sensitive: monterey.then(|| s.timeSensitiveSetting().0),
                direct_messages: monterey.then(|| s.directMessagesSetting().0),
            };
            let _sent = tx.try_send(raw);
        });
        Self::center().getNotificationSettingsWithCompletionHandler(&block);
        wait().ok_or_else(|| timed_out("notification settings"))
    }
}

/// `participant` as an `INPerson`, with its avatar from the host's files.
fn person(shown: &Shown<'_>, participant: &crate::Participant, is_me: bool) -> Retained<INPerson> {
    let key = ns(&participant.key);
    let avatar = participant.avatar.as_ref().and_then(|avatar| match avatar {
        crate::NotificationImage::Named(name) => {
            // SAFETY: a plain image name.
            Some(unsafe { INImage::imageNamed(&ns(name)) })
        }
        _ => shown
            .images
            .get(&ImageSlot::Avatar(participant.key.clone()))
            .and_then(|path| std::fs::read(path).ok())
            .map(|bytes| {
                // SAFETY: the image bytes are copied into the data.
                unsafe { INImage::imageWithImageData(&NSData::with_bytes(&bytes)) }
            }),
    });
    // SAFETY: a freshly allocated handle with a string value.
    let handle = unsafe {
        INPersonHandle::initWithValue_type(
            INPersonHandle::alloc(),
            Some(&key),
            INPersonHandleType::Unknown,
        )
    };
    // SAFETY: a freshly allocated person; every argument is a valid
    // object or `None` (macOS 12 initializer, gated by the caller).
    unsafe {
        INPerson::initWithPersonHandle_nameComponents_displayName_image_contactIdentifier_customIdentifier_isMe(
                INPerson::alloc(),
                &handle,
                None,
                Some(&ns(&participant.name)),
                avatar.as_deref(),
                None,
                Some(&key),
                is_me,
            )
    }
}

/// Builds a communication notification: the sender as an `INPerson`, an
/// incoming `INSendMessageIntent` donated, then
/// `contentByUpdatingWithProvider:error:`. `None` when any step fails
/// (no entitlement, donation refused).
fn communication(
    content: &UNMutableNotificationContent,
    shown: &Shown<'_>,
    conversation: &crate::Conversation,
) -> Option<Retained<UNNotificationContent>> {
    let n = shown.notification;
    let last = conversation.messages.last()?;
    let sender = last.sender.as_ref()?;
    let person = |participant: &crate::Participant, is_me: bool| person(shown, participant, is_me);
    let me = person(&conversation.user, true);
    let mut recipients = vec![me];
    if conversation.is_group {
        let mut seen = vec![sender.key.as_str(), conversation.user.key.as_str()];
        for message in &conversation.messages {
            if let Some(other) = &message.sender
                && !seen.contains(&other.key.as_str())
            {
                seen.push(&other.key);
                recipients.push(person(other, false));
            }
        }
    }
    let recipients = NSArray::from_retained_slice(&recipients);
    let group_name = conversation
        .title
        .as_ref()
        .filter(|_| conversation.is_group)
        .map(|title| {
            // SAFETY: a freshly allocated speakable string.
            unsafe {
                objc2_intents::INSpeakableString::initWithSpokenPhrase(
                    objc2_intents::INSpeakableString::alloc(),
                    &ns(title),
                )
            }
        });
    let sender = person(sender, false);
    // SAFETY: a freshly allocated intent; every argument is a valid
    // object or `None`.
    let intent = unsafe {
        INSendMessageIntent::initWithRecipients_outgoingMessageType_content_speakableGroupName_conversationIdentifier_serviceName_sender_attachments(
            INSendMessageIntent::alloc(),
            Some(&recipients),
            INOutgoingMessageType::OutgoingMessageText,
            Some(&ns(&last.text)),
            group_name.as_deref(),
            Some(&ns(n.id.as_str())),
            None,
            Some(&sender),
            None,
        )
    };
    // SAFETY: a freshly allocated interaction around a valid intent.
    let interaction =
        unsafe { INInteraction::initWithIntent_response(INInteraction::alloc(), &intent, None) };
    // SAFETY: setter of a plain enum property.
    unsafe { interaction.setDirection(INInteractionDirection::Incoming) };
    let (tx, wait) = waiter::<Option<(i64, String)>>();
    let block = RcBlock::new(move |error: *mut NSError| {
        let _sent = tx.try_send(error_message(error));
    });
    // SAFETY: the completion block matches the method's signature.
    unsafe { interaction.donateInteractionWithCompletion(Some(&block)) };
    match wait() {
        Some(None) => {}
        Some(Some((_, message))) => {
            tracing::debug!(%message, "notifications: interaction not donated");
            return None;
        }
        None => return None,
    }
    // `INSendMessageIntent` conforms to `UNNotificationContentProviding`
    // (Intents, macOS 12); objc2-intents does not declare the conformance,
    // so the method is sent directly.
    // SAFETY: `contentByUpdatingWithProvider:error:` takes an object
    // conforming to the protocol (the intent does) and an error out
    // pointer; it returns a new retained content or nil with the error.
    let updated: Result<Retained<UNNotificationContent>, Retained<NSError>> =
        unsafe { msg_send![content, contentByUpdatingWithProvider: &*intent, error: _] };
    match updated {
        Ok(updated) => Some(updated),
        Err(err) => {
            tracing::debug!(error = %err.localizedDescription(), "notifications: communication content refused");
            None
        }
    }
}

impl Backend for Mac {
    fn info(&self) -> BackendInfo {
        apple_map::info(&self.os_version, self.communication)
    }

    fn permission(&self) -> PermissionStatus {
        match self.settings() {
            Ok(raw) => apple_map::permission(raw),
            Err(err) => {
                tracing::debug!(%err, "notifications: settings not read");
                PermissionStatus::of(crate::PermissionState::NotDetermined)
            }
        }
    }

    fn request_permission(
        &self,
        request: &PermissionRequest,
    ) -> Result<PermissionStatus, NotificationError> {
        let (tx, wait) = waiter_for::<Option<(i64, String)>>(PROMPT_WAIT);
        let block = RcBlock::new(move |_granted: Bool, error: *mut NSError| {
            let _sent = tx.try_send(error_message(error));
        });
        Self::center().requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions(apple_map::authorization_options(*request)),
            &block,
        );
        if let Some(Some((code, message))) = wait() {
            return Err(NotificationError::Platform(PlatformError { code, message }));
        }
        Ok(apple_map::permission(self.settings()?))
    }

    fn open_settings(&self, _channel: Option<&ChannelId>) -> Result<(), NotificationError> {
        let Some(url) = NSURL::URLWithString(&ns(&apple_map::settings_url(&self.bundle_id))) else {
            return Err(NotificationError::Unsupported(crate::Feature::OpenSettings));
        };
        if NSWorkspace::sharedWorkspace().openURL(&url) {
            Ok(())
        } else {
            Err(NotificationError::Unsupported(crate::Feature::OpenSettings))
        }
    }

    fn show(&self, shown: &Shown<'_>) -> Result<(), NotificationError> {
        let quiet = matches!(shown.mode, super::ShowMode::Replace { alert: false });
        let content = self.content(shown, quiet);
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &ns(shown.notification.id.as_str()),
            &content,
            None,
        );
        let (tx, wait) = waiter::<Option<(i64, String)>>();
        let block = RcBlock::new(move |error: *mut NSError| {
            let _sent = tx.try_send(error_message(error));
        });
        Self::center().addNotificationRequest_withCompletionHandler(&request, Some(&block));
        match wait() {
            Some(None) => Ok(()),
            Some(Some((code, message))) => {
                Err(NotificationError::Platform(PlatformError { code, message }))
            }
            None => Err(timed_out("add request")),
        }
    }

    fn remove(&self, id: &NotificationId) -> Result<(), NotificationError> {
        let ids = Self::ids(&[id.as_str()]);
        let center = Self::center();
        center.removeDeliveredNotificationsWithIdentifiers(&ids);
        center.removePendingNotificationRequestsWithIdentifiers(&ids);
        Ok(())
    }

    fn delivered(&self) -> Result<Vec<DeliveredNotification>, NotificationError> {
        let (tx, wait) = waiter::<Vec<DeliveredNotification>>();
        let block = RcBlock::new(move |list: NonNull<NSArray<UNNotification>>| {
            // SAFETY: the framework passes a valid array for the duration
            // of the completion handler.
            let list = unsafe { list.as_ref() };
            let out = list
                .iter()
                .filter_map(|notification| delivered_entry(&notification))
                .collect();
            let _sent = tx.try_send(out);
        });
        Self::center().getDeliveredNotificationsWithCompletionHandler(&block);
        wait().ok_or_else(|| timed_out("delivered notifications"))
    }
}

/// One of this plugin's delivered notifications, `None` for others.
fn delivered_entry(notification: &UNNotification) -> Option<DeliveredNotification> {
    let content = notification.request().content();
    let id = string_value(&content.userInfo(), keys::ID)?;
    let id = NotificationId::try_from(id.as_str()).ok()?;
    let text = |value: Retained<NSString>| Some(value.to_string()).filter(|text| !text.is_empty());
    #[expect(
        clippy::cast_possible_truncation,
        reason = "milliseconds since 1970 fit in i64 for any real date"
    )]
    let delivered_ms = (notification.date().timeIntervalSince1970() * 1000.0) as i64;
    Some(DeliveredNotification {
        id,
        title: text(content.title()),
        body: text(content.body()),
        group: text(content.threadIdentifier()).map(|value| GroupId { value }),
        delivered_ms: Some(delivered_ms),
    })
}

fn string_value(info: &NSDictionary, key: &str) -> Option<String> {
    let key = ns(key);
    info.objectForKey(key.as_ref())?
        .downcast::<NSString>()
        .ok()
        .map(|value| value.to_string())
}

/// What the delegate needs.
struct DelegateIvars {
    sink: BackendSink,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the delegate does
    // not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "IstmoNotificationsDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            notification: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            let info = notification.request().content().userInfo();
            let key = ns(keys::PRESENT);
            let bits = info
                .objectForKey(key.as_ref())
                .and_then(|value| value.downcast::<NSNumber>().ok())
                .map_or(apple_map::DEFAULT_PRESENTATION, |value| value.as_u64());
            completion.call((UNNotificationPresentationOptions(
                usize::try_from(bits).unwrap_or_default(),
            ),));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            self.respond(response);
            completion.call(());
        }
    }
);

impl Delegate {
    fn new(sink: BackendSink) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { sink });
        // SAFETY: `NSObject`'s `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }

    fn respond(&self, response: &UNNotificationResponse) {
        let info = response.notification().request().content().userInfo();
        let Some(id) =
            string_value(&info, keys::ID).and_then(|id| NotificationId::try_from(id.as_str()).ok())
        else {
            // Not one of this plugin's.
            return;
        };
        let sink = &self.ivars().sink;
        let key = match apple_map::response_kind(&response.actionIdentifier().to_string()) {
            ResponseKind::Dismissed => {
                sink.dismissed(&id, DismissReason::User);
                return;
            }
            ResponseKind::Opened => None,
            ResponseKind::Button(key) => Some(key),
        };
        let any: &AnyObject = response.as_ref();
        let reply = any
            .downcast_ref::<UNTextInputNotificationResponse>()
            .map(|text| text.userText().to_string());
        sink.activated(Activation {
            id,
            key,
            reply,
            launched_app: LAUNCHED_BY_NOTIFICATION.swap(false, Ordering::AcqRel),
            carried: Some(Carried {
                user_data: string_value(&info, keys::USER_DATA),
                url: string_value(&info, keys::URL),
            }),
        });
    }
}
