//! Toast XML for one notification, as a pure function of the
//! notification, its [`Rendered`] form, its image files and the
//! Windows build.
//!
//! Layout (`ToastGeneric`): title, subtitle and body as the three text
//! elements, the status text as attribution when there is no progress
//! bar, the icon as `appLogoOverride` (circle crop for avatars of
//! conversations and calls), the picture as `hero`, the latest message
//! image inline, the progress bar, then audio, inputs and buttons, and a
//! `header` for groups.
//!
//! Shown toasts bind every top-level text and the progress bar
//! (`{title}`, `{progressValue}`, ...) so a silent update is a
//! `ToastNotifier.Update` of the values; [`Toast::template`] tells
//! whether two toasts differ only in those values. Scheduled toasts
//! cannot carry data and are written with the values in place.
//!
//! The whole XML must stay within [`MAX_XML`]; [`build_fitting`] drops
//! carried user data from the buttons first, then from the body, then
//! the carried URLs, before refusing.

use std::path::Path;

use super::args::Args;
use super::ident::file_uri;
use crate::desktop::{ImageFiles, ImageSlot};
use crate::render::{Input, Rendered, RenderedAction};
use crate::{
    ActionKind, CallKind, Category, Interruption, InvalidField, Notification, NotificationError,
    Problem, Sound, Style,
};

/// Toast payload limit (Learn: "the entire XML notification content
/// cannot exceed 5 KB").
pub(crate) const MAX_XML: usize = 5 * 1024;
/// First build with the `urgent` scenario (Learn, app notification
/// content: "Requires ... Build 22546 or later").
pub(crate) const BUILD_URGENT: u32 = 22_546;
/// First build with styled (`hint-buttonStyle`) buttons (Windows 11).
pub(crate) const BUILD_BUTTON_STYLE: u32 = 22_000;

/// Which carried fields go into the arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Carry {
    /// User data and URL on the body and every button.
    All,
    /// User data on the body only; URLs everywhere.
    BodyData,
    /// URLs only.
    Urls,
    /// Ids and keys only.
    Nothing,
}

/// Options of one toast.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    /// The Windows build (feature gates).
    pub(crate) build: u32,
    /// Bind texts and progress to data values (shown toasts) or write
    /// them in place (scheduled toasts).
    pub(crate) bind: bool,
    /// Fire time of a scheduled toast (marks its arguments).
    pub(crate) fire_ms: Option<i64>,
    /// Carried fields.
    pub(crate) carry: Carry,
}

/// A built toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Toast {
    /// The XML document.
    pub(crate) xml: String,
    /// Data values of the bound fields (empty when not bound).
    pub(crate) values: Vec<(&'static str, String)>,
    /// Hash of the XML: equal hashes = only the values differ.
    pub(crate) template: u64,
}

/// The escaped form of `c`: `Some("")` for characters XML 1.0 cannot
/// carry (controls but tab / LF / CR, the noncharacters U+FFFE and
/// U+FFFF; Rust strings hold no lone surrogates), `None` when it goes
/// as is.
fn escaped(c: char) -> Option<&'static str> {
    match c {
        '&' => Some("&amp;"),
        '<' => Some("&lt;"),
        '>' => Some("&gt;"),
        '"' => Some("&quot;"),
        '\'' => Some("&apos;"),
        '\n' | '\r' | '\t' => None,
        '\u{FFFE}' | '\u{FFFF}' => Some(""),
        c if c.is_control() => Some(""),
        _ => None,
    }
}

/// XML escaping for attribute values and text.
fn escape(out: &mut String, value: &str) {
    for c in value.chars() {
        match escaped(c) {
            Some(text) => out.push_str(text),
            None => out.push(c),
        }
    }
}

/// `value` without the characters XML cannot carry (bound values are
/// put into the XML by Windows).
fn xml_text(value: &str) -> String {
    value.chars().filter(|&c| escaped(c) != Some("")).collect()
}

/// Bytes `value` takes once escaped.
fn escaped_len(value: &str) -> usize {
    value
        .chars()
        .map(|c| escaped(c).map_or(c.len_utf8(), str::len))
        .sum()
}

/// Writes ` name="value"`.
fn attr(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    escape(out, value);
    out.push('"');
}

/// One text slot: bound (`{key}` + value) or in place.
struct Fields {
    bind: bool,
    values: Vec<(&'static str, String)>,
}

impl Fields {
    /// The text to write for `value` under `key`.
    fn slot(&mut self, key: &'static str, value: &str) -> String {
        if self.bind {
            self.values.push((key, xml_text(value)));
            format!("{{{key}}}")
        } else {
            value.to_owned()
        }
    }
}

/// The `appLogoOverride` image and whether it is circle-cropped: the
/// caller's avatar, the latest message sender's avatar, else the icon.
fn logo(n: &Notification) -> Option<(ImageSlot, bool)> {
    match &n.style {
        Style::Call(call) if call.caller.avatar.is_some() => {
            return Some((ImageSlot::Avatar(call.caller.key.clone()), true));
        }
        Style::Messaging(conversation) => {
            if let Some(sender) = conversation
                .messages
                .iter()
                .rev()
                .filter_map(|message| message.sender.as_ref())
                .find(|sender| sender.avatar.is_some())
            {
                return Some((ImageSlot::Avatar(sender.key.clone()), true));
            }
        }
        _ => {}
    }
    n.icon.as_ref().map(|_| (ImageSlot::Icon, false))
}

/// The `scenario` attribute.
const fn scenario(n: &Notification, rendered: &Rendered, build: u32) -> Option<&'static str> {
    if let Style::Call(call) = &n.style
        && matches!(call.kind, CallKind::Incoming | CallKind::Screening)
    {
        return Some("incomingCall");
    }
    // Alarms and reminders need a button, else Windows shows a normal
    // toast; reminders also need one that activates in the background.
    let buttons = !rendered.actions.is_empty();
    match n.category {
        Some(Category::Alarm) if buttons => return Some("alarm"),
        Some(Category::Reminder) if buttons => return Some("reminder"),
        _ => {}
    }
    if n.ongoing && buttons {
        return Some("reminder");
    }
    if matches!(
        n.interruption,
        Some(Interruption::Urgent | Interruption::Critical)
    ) && build >= BUILD_URGENT
    {
        return Some("urgent");
    }
    None
}

/// The `audio` element.
fn audio(out: &mut String, n: &Notification) {
    let silent = matches!(n.sound, Some(Sound::Silent))
        || matches!(n.interruption, Some(Interruption::Passive));
    if silent {
        out.push_str("<audio silent=\"true\"/>");
        return;
    }
    // Unpackaged apps can only play the system sounds; bundled sound
    // names fall back to the default sound.
    if let Some(Sound::Named(name)) = &n.sound
        && name.starts_with("ms-winsoundevent:")
    {
        out.push_str("<audio");
        attr(out, "src", name);
        out.push_str("/>");
    }
}

/// `<image .../>` for `slot`, when it has a local file.
fn image(out: &mut String, images: &ImageFiles, slot: &ImageSlot, extra: &[(&str, &str)]) {
    let Some(uri) = images.get(slot).and_then(Path::to_str).and_then(file_uri) else {
        return;
    };
    out.push_str("<image");
    for (name, value) in extra {
        attr(out, name, value);
    }
    attr(out, "src", &uri);
    out.push_str("/>");
}

/// The progress value as the bar takes it.
fn progress_value(value: Option<f32>) -> String {
    value.map_or_else(
        || "indeterminate".to_owned(),
        |value| {
            let value = if value.is_finite() {
                value.clamp(0.0, 1.0)
            } else {
                0.0
            };
            format!("{value:.4}")
        },
    )
}

/// Arguments of `rendered` (or of the body when `None`).
fn arguments(n: &Notification, action: Option<&RenderedAction>, options: Options) -> String {
    let action_url = action.and_then(|rendered| {
        let id = rendered
            .key
            .split_once('#')
            .map_or(rendered.key.as_str(), |(id, _)| id);
        n.actions
            .iter()
            .find(|a| a.id.as_str() == id)
            .and_then(|a| a.url.clone())
    });
    let user_data = match (options.carry, action) {
        (Carry::All, _) | (Carry::BodyData, None) => n.user_data.clone(),
        _ => None,
    };
    let url = match options.carry {
        Carry::Nothing => None,
        _ => action_url.or_else(|| n.url.clone()),
    };
    Args {
        id: n.id.value.clone(),
        key: action.map(|rendered| rendered.key.clone()),
        user_data,
        url,
        fire_ms: options.fire_ms,
    }
    .encode()
}

/// The toast of `n`.
pub(crate) fn build(
    n: &Notification,
    rendered: &Rendered,
    images: &ImageFiles,
    options: Options,
) -> Toast {
    let mut out = String::with_capacity(1024);
    let mut fields = Fields {
        bind: options.bind,
        values: Vec::new(),
    };
    let scenario = scenario(n, rendered, options.build);
    let styled = options.build >= BUILD_BUTTON_STYLE
        && rendered.actions.iter().any(|action| action.destructive);

    out.push_str("<toast");
    attr(&mut out, "launch", &arguments(n, None, options));
    if let Some(ms) = n.timestamp_ms
        && let Ok(at) = jiff::Timestamp::from_millisecond(ms)
    {
        attr(
            &mut out,
            "displayTimestamp",
            &at.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
        );
    }
    if let Some(scenario) = scenario {
        attr(&mut out, "scenario", scenario);
    }
    if matches!(
        n.interruption,
        Some(Interruption::High | Interruption::Urgent | Interruption::Critical)
    ) {
        attr(&mut out, "duration", "long");
    }
    if styled {
        attr(&mut out, "useButtonStyle", "true");
    }
    out.push_str("><visual><binding template=\"ToastGeneric\">");

    let call = scenario == Some("incomingCall");
    let text = |out: &mut String, fields: &mut Fields, key: &'static str, value: &str| {
        out.push_str("<text");
        if call {
            attr(out, "hint-callScenarioCenterAlign", "true");
        }
        out.push('>');
        let slot = fields.slot(key, value);
        escape(out, &slot);
        out.push_str("</text>");
    };
    text(&mut out, &mut fields, "title", &n.title);
    if let Some(subtitle) = &n.subtitle {
        text(&mut out, &mut fields, "subtitle", subtitle);
    }
    if let Some(body) = &rendered.body {
        text(&mut out, &mut fields, "body", body);
    }
    write_extras(&mut out, &mut fields, n, images);
    out.push_str("</binding></visual>");
    audio(&mut out, n);

    if !rendered.actions.is_empty() {
        out.push_str("<actions>");
        for action in &rendered.actions {
            write_input(&mut out, n, action);
        }
        for action in &rendered.actions {
            write_button(&mut out, n, action, images, options, styled);
        }
        out.push_str("</actions>");
    }
    if let Some(group) = &n.group {
        out.push_str("<header");
        attr(&mut out, "id", group.id.as_str());
        attr(
            &mut out,
            "title",
            group.title.as_deref().unwrap_or(group.id.as_str()),
        );
        attr(&mut out, "arguments", &arguments(n, None, options));
        attr(&mut out, "activationType", "foreground");
        out.push_str("/>");
    }
    out.push_str("</toast>");
    let template = super::ident::fnv1a(out.as_bytes());
    Toast {
        xml: out,
        values: fields.values,
        template,
    }
}

/// Attribution, images and the progress bar.
fn write_extras(out: &mut String, fields: &mut Fields, n: &Notification, images: &ImageFiles) {
    if n.progress.is_none()
        && let Some(status) = &n.status_text
    {
        out.push_str("<text placement=\"attribution\">");
        let slot = fields.slot("attribution", status);
        escape(out, &slot);
        out.push_str("</text>");
    }
    if let Some((slot, circle)) = logo(n) {
        let crop: &[(&str, &str)] = if circle {
            &[("placement", "appLogoOverride"), ("hint-crop", "circle")]
        } else {
            &[("placement", "appLogoOverride")]
        };
        image(out, images, &slot, crop);
    }
    match &n.style {
        Style::Picture(_) => image(out, images, &ImageSlot::Picture, &[("placement", "hero")]),
        Style::Messaging(conversation) => {
            if let Some(index) = conversation.messages.len().checked_sub(1)
                && conversation.messages[index].image.is_some()
            {
                image(out, images, &ImageSlot::Message(index), &[]);
            }
        }
        _ => {}
    }
    if let Some(progress) = &n.progress {
        let status = n
            .status_text
            .as_deref()
            .or(progress.label.as_deref())
            .unwrap_or("");
        let title = n.status_text.as_ref().and(progress.label.as_deref());
        out.push_str("<progress");
        if let Some(title) = title {
            let slot = fields.slot("progressTitle", title);
            attr(out, "title", &slot);
        }
        let slot = fields.slot("progressValue", &progress_value(progress.value));
        attr(out, "value", &slot);
        let slot = fields.slot("progressStatus", status);
        attr(out, "status", &slot);
        out.push_str("/>");
    }
}
/// The reply options of the action behind `rendered`.
fn reply_options<'a>(
    n: &'a Notification,
    rendered: &RenderedAction,
) -> Option<&'a crate::ReplyOptions> {
    n.actions
        .iter()
        .find(|action| action.id.as_str() == rendered.key)
        .and_then(|action| match &action.kind {
            ActionKind::Reply(options) => Some(options),
            _ => None,
        })
}

/// The input of `action`, if it takes one. The input id is the action
/// key; a selection's item ids are the choices themselves, so the
/// activation hands back the chosen text.
fn write_input(out: &mut String, n: &Notification, action: &RenderedAction) {
    match &action.input {
        Input::Reply => {
            out.push_str("<input type=\"text\"");
            attr(out, "id", &action.key);
            if let Some(placeholder) =
                reply_options(n, action).and_then(|o| o.placeholder.as_deref())
            {
                attr(out, "placeHolderContent", placeholder);
            }
            out.push_str("/>");
        }
        Input::Choices(choices) => {
            out.push_str("<input type=\"selection\"");
            attr(out, "id", &action.key);
            if let Some(first) = choices.first() {
                attr(out, "defaultInput", first);
            }
            out.push('>');
            for choice in choices {
                out.push_str("<selection");
                attr(out, "id", choice);
                attr(out, "content", choice);
                out.push_str("/>");
            }
            out.push_str("</input>");
        }
        Input::None | Input::Requested => {}
    }
}

fn write_button(
    out: &mut String,
    n: &Notification,
    action: &RenderedAction,
    images: &ImageFiles,
    options: Options,
    styled: bool,
) {
    let title = match action.input {
        Input::Reply => reply_options(n, action)
            .and_then(|o| o.submit_title.as_deref())
            .unwrap_or(&action.title),
        _ => &action.title,
    };
    out.push_str("<action");
    attr(out, "content", title);
    attr(out, "arguments", &arguments(n, Some(action), options));
    attr(
        out,
        "activationType",
        if action.opens_app {
            "foreground"
        } else {
            "background"
        },
    );
    if matches!(action.input, Input::Reply | Input::Choices(_)) {
        attr(out, "hint-inputId", &action.key);
    }
    let id = action
        .key
        .split_once('#')
        .map_or(action.key.as_str(), |(id, _)| id);
    if let Some(uri) = n
        .actions
        .iter()
        .find(|a| a.id.as_str() == id)
        .and_then(|a| images.get(&ImageSlot::Action(a.id.clone())))
        .and_then(Path::to_str)
        .and_then(file_uri)
    {
        attr(out, "imageUri", &uri);
    }
    if styled && action.destructive {
        attr(out, "hint-buttonStyle", "Critical");
    }
    out.push_str("/>");
}

/// [`build`] with the carried fields reduced until the XML fits
/// [`MAX_XML`]. Bound values count as if written in place (Windows
/// substitutes them into the XML it checks).
pub(crate) fn build_fitting(
    n: &Notification,
    rendered: &Rendered,
    images: &ImageFiles,
    mut options: Options,
) -> Result<Toast, NotificationError> {
    for carry in [Carry::All, Carry::BodyData, Carry::Urls, Carry::Nothing] {
        options.carry = carry;
        let toast = build(n, rendered, images, options);
        let bound: usize = toast.values.iter().map(|(_, v)| escaped_len(v)).sum();
        if toast.xml.len().saturating_add(bound) <= MAX_XML {
            if carry != Carry::All {
                tracing::debug!(id = %n.id, ?carry, "notifications: toast too large; carried fields reduced");
            }
            return Ok(toast);
        }
    }
    Err(InvalidField::new("notification", Problem::TooLarge).into())
}

/// The images of `n` that are already files ([`crate::NotificationImage::File`]):
/// all a scheduled toast can use, since cached copies of image bytes
/// live only while a shown notification holds them.
pub(crate) fn file_images(n: &Notification) -> ImageFiles {
    let mut entries = Vec::new();
    let mut add = |slot: ImageSlot, image: Option<&crate::NotificationImage>| {
        if let Some(crate::NotificationImage::File(path)) = image
            && !entries.iter().any(|(have, _)| *have == slot)
        {
            entries.push((slot, std::path::PathBuf::from(path)));
        }
    };
    add(ImageSlot::Icon, n.icon.as_ref());
    match &n.style {
        Style::Picture(image) => add(ImageSlot::Picture, Some(image)),
        Style::Call(call) => add(
            ImageSlot::Avatar(call.caller.key.clone()),
            call.caller.avatar.as_ref(),
        ),
        Style::Messaging(conversation) => {
            for (index, message) in conversation.messages.iter().enumerate() {
                if let Some(sender) = &message.sender {
                    add(
                        ImageSlot::Avatar(sender.key.clone()),
                        sender.avatar.as_ref(),
                    );
                }
                add(ImageSlot::Message(index), message.image.as_ref());
            }
        }
        _ => {}
    }
    for action in &n.actions {
        add(ImageSlot::Action(action.id.clone()), action.icon.as_ref());
    }
    ImageFiles { entries }
}

/// Writes a debug summary (tests).
#[cfg(test)]
pub(crate) fn values_text(toast: &Toast) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (key, value) in &toast.values {
        let _ = writeln!(out, "{key}={value}");
    }
    out
}

#[cfg(test)]
mod tests;
