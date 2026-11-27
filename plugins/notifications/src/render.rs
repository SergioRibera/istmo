//! Fallbacks for what a backend cannot show natively, as pure functions
//! of the notification and the backend's [`RenderLimits`].
//!
//! * Progress without a bar: `<label> 42%` (or `<label>` when
//!   indeterminate) as the last body line.
//! * Styles without a layout: big text replaces the body, inbox lines
//!   and the last messages of a conversation become body lines.
//! * A subtitle without a slot: the first body line.
//! * Inline replies without a field, and choices without a picker:
//!   buttons. A reply button opens the app with
//!   [`NotificationResponse::input_requested`]; each choice becomes its
//!   own button whose response carries the choice as the reply.
//! * More actions than the platform shows: the first `max - 1` stay and
//!   a "More" action ([`ActionId::MORE`]) opens the app with
//!   [`ResponseAction::More`] and the rest in
//!   [`NotificationResponse::overflow_actions`].
//! * Calls: the plugin's answer / decline / hang-up actions are added
//!   when the app gave none.
//!
//! Each [`RenderedAction`] has a [`key`](RenderedAction::key): the
//! identifier the backend gives the platform action. [`Rendered::respond`]
//! turns a key coming back from the platform into the response. Keys are
//! action ids, [`ActionId::MORE`], or `<action id>#<choice index>` (`#`
//! is outside the id charset).

use crate::{
    Action, ActionId, ActionKind, CallKind, Capabilities, Feature, Notification,
    NotificationResponse, Progress, ResponseAction, Style, Support,
};

/// Messages of a conversation shown as body lines where there is no
/// messaging layout.
pub const SIMULATED_MESSAGES: usize = 5;

/// What a backend shows natively; everything else is simulated here.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per independent platform feature"
)]
pub struct RenderLimits {
    /// Most action buttons on one notification.
    pub max_actions: u32,
    /// A native subtitle slot.
    pub subtitle: bool,
    /// A native progress bar.
    pub progress_bar: bool,
    /// A native inline reply field.
    pub inline_reply: bool,
    /// A native choice picker (Android `setChoices`, Windows selection box).
    pub choices: bool,
    /// A native expanded big-text layout.
    pub big_text: bool,
    /// A native inbox layout.
    pub inbox: bool,
    /// A native conversation layout.
    pub messaging: bool,
    /// A native call layout (the platform draws its own buttons).
    pub call: bool,
    /// Labels of the actions the plugin adds.
    pub labels: Labels,
}

impl RenderLimits {
    /// Limits read from `caps`: a feature counts as native only when it
    /// is [`Support::Native`]. Subtitle, big text and inbox have no
    /// [`Feature`]; they are off (set them by hand).
    #[must_use]
    pub fn from_capabilities(caps: &Capabilities) -> Self {
        let native = |feature| caps.support(feature) == Support::Native;
        Self {
            max_actions: caps.max_actions,
            subtitle: false,
            progress_bar: native(Feature::ProgressBar),
            inline_reply: native(Feature::InlineReply),
            choices: native(Feature::Choices),
            big_text: false,
            inbox: false,
            messaging: native(Feature::Messaging),
            call: native(Feature::CallStyle),
            labels: Labels::default(),
        }
    }
}

impl Default for RenderLimits {
    /// Nothing native, three buttons.
    fn default() -> Self {
        Self {
            max_actions: 3,
            subtitle: false,
            progress_bar: false,
            inline_reply: false,
            choices: false,
            big_text: false,
            inbox: false,
            messaging: false,
            call: false,
            labels: Labels::default(),
        }
    }
}

/// Labels of the actions the plugin adds (English by default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Labels {
    /// The overflow action.
    pub more: String,
    /// Answer a call.
    pub answer: String,
    /// Decline a call.
    pub decline: String,
    /// Hang up a call.
    pub hang_up: String,
}

impl Default for Labels {
    fn default() -> Self {
        Self {
            more: "More".to_owned(),
            answer: "Answer".to_owned(),
            decline: "Decline".to_owned(),
            hang_up: "Hang up".to_owned(),
        }
    }
}

/// How a rendered action takes input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// None (a button).
    None,
    /// A native reply field.
    Reply,
    /// A native choice picker.
    Choices(Vec<String>),
    /// A simulated reply: the button opens the app, which shows its field.
    Requested,
}

/// One button (or field) the backend shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedAction {
    /// Platform identifier of the action; comes back to [`Rendered::respond`].
    pub key: String,
    /// Label.
    pub title: String,
    /// Input it takes.
    pub input: Input,
    /// Brings the app forward.
    pub opens_app: bool,
    /// Shown as destructive.
    pub destructive: bool,
    /// Needs the device unlocked.
    pub requires_unlock: bool,
    /// Snooze delay, when this is a snooze (handled by the host).
    pub snooze_ms: Option<u64>,
}

/// What the backend shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rendered {
    /// Body text with every simulated line, `None` when empty.
    pub body: Option<String>,
    /// Buttons, in order (the "More" action last).
    pub actions: Vec<RenderedAction>,
    /// Actions that did not fit.
    pub overflow: Vec<Action>,
}

/// What a platform activation means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// Publish this response.
    Respond(NotificationResponse),
    /// Hide the notification and show it again after this many
    /// milliseconds.
    Snooze(u64),
}

/// The simulated progress line: `<label> 42%`, `42%`, or `<label>` when
/// indeterminate; `None` for an indeterminate bar without a label.
#[must_use]
pub fn progress_text(progress: &Progress) -> Option<String> {
    let percent = progress.value.map(|value| {
        let value = if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        };
        // Truncates: a bar at 99.9 % is not done.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "value is clamped to 0..=1, so the product fits in u8"
        )]
        let percent = (value * 100.0).floor() as u8;
        percent
    });
    match (progress.label.as_deref(), percent) {
        (Some(label), Some(percent)) => Some(format!("{label} {percent}%")),
        (None, Some(percent)) => Some(format!("{percent}%")),
        (Some(label), None) => Some(label.to_owned()),
        (None, None) => None,
    }
}

/// The body lines `n` needs on a backend with `limits`, joined by `\n`.
#[must_use]
pub fn body(n: &Notification, limits: &RenderLimits) -> Option<String> {
    let mut lines: Vec<&str> = Vec::new();
    if !limits.subtitle
        && let Some(subtitle) = n.subtitle.as_deref()
    {
        lines.push(subtitle);
    }
    let mut owned: Vec<String> = Vec::new();
    match &n.style {
        Style::BigText(text) if !limits.big_text => lines.push(text),
        Style::Inbox(entries) if !limits.inbox => {
            lines.extend(n.body.as_deref());
            lines.extend(entries.iter().map(String::as_str));
        }
        Style::Messaging(conversation) if !limits.messaging => {
            lines.extend(n.body.as_deref());
            let skip = conversation
                .messages
                .len()
                .saturating_sub(SIMULATED_MESSAGES);
            for message in &conversation.messages[skip..] {
                let name = message
                    .sender
                    .as_ref()
                    .map_or(conversation.user.name.as_str(), |sender| {
                        sender.name.as_str()
                    });
                owned.push(format!("{name}: {}", message.text));
            }
        }
        _ => lines.extend(n.body.as_deref()),
    }
    let progress = if limits.progress_bar {
        None
    } else {
        n.progress.as_ref().and_then(progress_text)
    };
    let mut out = String::new();
    let all = lines
        .into_iter()
        .chain(owned.iter().map(String::as_str))
        .chain(progress.as_deref())
        .filter(|line| !line.is_empty());
    for line in all {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    (!out.is_empty()).then_some(out)
}

/// The ids of the actions the plugin adds to calls.
const CALL_ACTIONS: [&str; 3] = [
    ActionId::CALL_ANSWER,
    ActionId::CALL_DECLINE,
    ActionId::CALL_HANG_UP,
];

/// Platform key of choice `index` of `action`.
fn choice_key(action: &ActionId, index: usize) -> String {
    format!("{}#{index}", action.as_str())
}

/// The actions the plugin adds to a call that has none of its own.
fn call_actions(kind: CallKind, labels: &Labels) -> Vec<Action> {
    let button = |id: &'static str, title: &str, destructive: bool| Action {
        id: ActionId::reserved(id),
        title: title.to_owned(),
        kind: ActionKind::Button,
        opens_app: true,
        destructive,
        requires_unlock: false,
        url: None,
        icon: None,
    };
    match kind {
        CallKind::Incoming | CallKind::Screening => vec![
            button(ActionId::CALL_DECLINE, &labels.decline, true),
            button(ActionId::CALL_ANSWER, &labels.answer, false),
        ],
        CallKind::Ongoing => vec![button(ActionId::CALL_HANG_UP, &labels.hang_up, true)],
    }
}

/// The actions of `n` with the call actions added when needed.
fn effective_actions<'a>(
    n: &'a Notification,
    limits: &RenderLimits,
) -> std::borrow::Cow<'a, [Action]> {
    if let Style::Call(call) = &n.style
        && !n.actions.iter().any(|action| action.id.is_reserved())
    {
        let mut all = call_actions(call.kind, &limits.labels);
        all.extend(n.actions.iter().cloned());
        return std::borrow::Cow::Owned(all);
    }
    std::borrow::Cow::Borrowed(&n.actions)
}

/// One action as the buttons it becomes.
fn expand(action: &Action, limits: &RenderLimits, out: &mut Vec<RenderedAction>) {
    let base = |key: String, title: &str, input: Input, opens_app: bool| RenderedAction {
        key,
        title: title.to_owned(),
        input,
        opens_app,
        destructive: action.destructive,
        requires_unlock: action.requires_unlock,
        snooze_ms: None,
    };
    match &action.kind {
        ActionKind::Button => out.push(base(
            action.id.value.clone(),
            &action.title,
            Input::None,
            action.opens_app,
        )),
        ActionKind::Reply(_) if limits.inline_reply => out.push(base(
            action.id.value.clone(),
            &action.title,
            Input::Reply,
            action.opens_app,
        )),
        ActionKind::Reply(_) => out.push(base(
            action.id.value.clone(),
            &action.title,
            Input::Requested,
            true,
        )),
        ActionKind::Select(choices) if limits.choices => out.push(base(
            action.id.value.clone(),
            &action.title,
            Input::Choices(choices.clone()),
            action.opens_app,
        )),
        ActionKind::Select(choices) => {
            for (index, choice) in choices.iter().enumerate() {
                out.push(base(
                    choice_key(&action.id, index),
                    choice,
                    Input::None,
                    action.opens_app,
                ));
            }
        }
        ActionKind::Snooze(ms) => {
            let mut snooze = base(action.id.value.clone(), &action.title, Input::None, false);
            snooze.snooze_ms = Some(*ms);
            out.push(snooze);
        }
    }
}

/// Everything `n` needs on a backend with `limits`.
#[must_use]
pub fn render(n: &Notification, limits: &RenderLimits) -> Rendered {
    let actions = effective_actions(n, limits);
    // A native call layout draws its own buttons from the call actions;
    // they still go through here so their keys resolve.
    let mut buttons = Vec::with_capacity(actions.len());
    // Index of the first button of each action, to know which actions
    // overflow.
    let mut first_button = Vec::with_capacity(actions.len());
    for action in actions.iter() {
        first_button.push(buttons.len());
        expand(action, limits, &mut buttons);
    }
    let max = usize::try_from(limits.max_actions).unwrap_or(usize::MAX);
    let mut overflow = Vec::new();
    if buttons.len() > max {
        if max == 0 {
            buttons.clear();
            overflow = actions.into_owned();
        } else {
            let keep = max - 1;
            // Whole actions only: the first one that loses a button
            // overflows with every action after it.
            let end_of = |i: usize| first_button.get(i + 1).copied().unwrap_or(buttons.len());
            let cut = (0..actions.len())
                .take_while(|&i| end_of(i) <= keep)
                .count();
            let kept = first_button.get(cut).copied().unwrap_or(buttons.len());
            buttons.truncate(kept);
            overflow = actions[cut..].to_vec();
            buttons.push(RenderedAction {
                key: ActionId::MORE.to_owned(),
                title: limits.labels.more.clone(),
                input: Input::None,
                opens_app: true,
                destructive: false,
                requires_unlock: false,
                snooze_ms: None,
            });
        }
    }
    Rendered {
        body: body(n, limits),
        actions: buttons,
        overflow,
    }
}

impl Rendered {
    /// The rendered action with platform key `key`.
    #[must_use]
    pub fn action(&self, key: &str) -> Option<&RenderedAction> {
        self.actions.iter().find(|action| action.key == key)
    }

    /// The response to a platform activation of `n`: `key` is the
    /// rendered action's key (`None` = the notification itself), `reply`
    /// the text the platform handed over. `None` when `key` is unknown.
    #[must_use]
    pub fn respond(
        &self,
        n: &Notification,
        key: Option<&str>,
        reply: Option<String>,
        launched_app: bool,
    ) -> Option<Resolved> {
        let mut response = NotificationResponse {
            id: n.id.clone(),
            action: ResponseAction::Open,
            reply: None,
            input_requested: false,
            user_data: n.user_data.clone(),
            url: n.url.clone(),
            launched_app,
            overflow_actions: Vec::new(),
            ack: None,
        };
        let Some(key) = key else {
            return Some(Resolved::Respond(response));
        };
        if key == ActionId::MORE {
            response.action = ResponseAction::More;
            response.overflow_actions.clone_from(&self.overflow);
            return Some(Resolved::Respond(response));
        }
        let rendered = self.action(key)?;
        if let Some(ms) = rendered.snooze_ms {
            return Some(Resolved::Snooze(ms));
        }
        let (id, choice) = match key.split_once('#') {
            Some((id, index)) => (id, index.parse::<usize>().ok()),
            None => (key, None),
        };
        response.input_requested = rendered.input == Input::Requested;
        if let Some(action) = n.actions.iter().find(|action| action.id.as_str() == id) {
            response.action = ResponseAction::Button(action.id.clone());
            if let Some(url) = &action.url {
                response.url = Some(url.clone());
            }
            response.reply = match (choice, &action.kind) {
                (Some(index), ActionKind::Select(choices)) => choices.get(index).cloned(),
                _ => reply,
            };
        } else {
            // A call action the plugin added.
            let added = CALL_ACTIONS.iter().find(|call| **call == id)?;
            response.action = ResponseAction::Button(ActionId::reserved(added));
            response.reply = reply;
        }
        Some(Resolved::Respond(response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallInfo, ChatMessage, Conversation, Participant};

    fn n() -> Notification {
        Notification::new("n", "Title").unwrap()
    }

    fn keys(rendered: &Rendered) -> Vec<&str> {
        rendered.actions.iter().map(|a| a.key.as_str()).collect()
    }

    #[test]
    fn progress_becomes_the_last_line() {
        let bar = Progress::fraction(0.426).with_label("3 of 8 MB");
        assert_eq!(progress_text(&bar).as_deref(), Some("3 of 8 MB 42%"));
        assert_eq!(
            progress_text(&Progress::fraction(1.0)).as_deref(),
            Some("100%")
        );
        assert_eq!(progress_text(&Progress::indeterminate()), None);
        let n = n().with_body("Downloading").with_progress(bar);
        assert_eq!(
            body(&n, &RenderLimits::default()).as_deref(),
            Some("Downloading\n3 of 8 MB 42%")
        );
        let native = RenderLimits {
            progress_bar: true,
            ..RenderLimits::default()
        };
        assert_eq!(body(&n, &native).as_deref(), Some("Downloading"));
    }

    #[test]
    fn styles_become_body_lines() {
        let limits = RenderLimits::default();
        let big = n()
            .with_body("short")
            .with_style(Style::BigText("long".to_owned()));
        assert_eq!(body(&big, &limits).as_deref(), Some("long"));
        let inbox = n().with_style(Style::Inbox(vec!["a".to_owned(), "b".to_owned()]));
        assert_eq!(body(&inbox, &limits).as_deref(), Some("a\nb"));
        let ana = Participant::new("ana", "Ana");
        let mut conversation = Conversation::new(Participant::new("me", "Me"));
        for i in 0..7 {
            conversation
                .messages
                .push(ChatMessage::received(ana.clone(), format!("m{i}")));
        }
        conversation.messages.push(ChatMessage::sent("ok"));
        let chat = n().with_style(Style::Messaging(conversation));
        assert_eq!(
            body(&chat, &limits).as_deref(),
            Some("Ana: m3\nAna: m4\nAna: m5\nAna: m6\nMe: ok")
        );
        let subtitle = n().with_subtitle("sub").with_body("body");
        assert_eq!(body(&subtitle, &limits).as_deref(), Some("sub\nbody"));
        assert_eq!(body(&self::n(), &limits), None);
    }

    #[test]
    fn extra_actions_overflow_into_more() {
        let mut n = n();
        for id in ["a", "b", "c", "d"] {
            n = n.with_action(Action::button(id, id.to_uppercase()).unwrap());
        }
        let rendered = render(&n, &RenderLimits::default());
        assert_eq!(keys(&rendered), ["a", "b", ActionId::MORE]);
        let overflow: Vec<_> = rendered.overflow.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(overflow, ["c", "d"]);
        let Some(Resolved::Respond(more)) = rendered.respond(&n, Some(ActionId::MORE), None, false)
        else {
            panic!("more resolves")
        };
        assert_eq!(more.action, ResponseAction::More);
        assert_eq!(more.overflow_actions.len(), 2);
        // Exactly the maximum: no "More".
        let three = Notification {
            actions: n.actions[..3].to_vec(),
            ..n.clone()
        };
        assert_eq!(
            keys(&render(&three, &RenderLimits::default())),
            ["a", "b", "c"]
        );
        let none = RenderLimits {
            max_actions: 0,
            ..RenderLimits::default()
        };
        let rendered = render(&n, &none);
        assert!(rendered.actions.is_empty());
        assert_eq!(rendered.overflow.len(), 4);
    }

    #[test]
    fn choices_become_buttons_and_answer_with_the_choice() {
        let n = n().with_action(Action::select("rsvp", "RSVP", ["Yes", "No"]).unwrap());
        let rendered = render(&n, &RenderLimits::default());
        assert_eq!(keys(&rendered), ["rsvp#0", "rsvp#1"]);
        assert_eq!(rendered.actions[1].title, "No");
        let Some(Resolved::Respond(response)) = rendered.respond(&n, Some("rsvp#1"), None, false)
        else {
            panic!("choice resolves")
        };
        assert_eq!(
            response.action,
            ResponseAction::Button(ActionId::try_from("rsvp").unwrap())
        );
        assert_eq!(response.reply.as_deref(), Some("No"));
        let native = RenderLimits {
            choices: true,
            ..RenderLimits::default()
        };
        let rendered = render(&n, &native);
        assert_eq!(keys(&rendered), ["rsvp"]);
        assert_eq!(
            rendered.actions[0].input,
            Input::Choices(vec!["Yes".to_owned(), "No".to_owned()])
        );
    }

    #[test]
    fn a_choice_set_that_does_not_fit_overflows_whole() {
        let n = n()
            .with_action(Action::button("a", "A").unwrap())
            .with_action(Action::select("s", "S", ["1", "2", "3"]).unwrap());
        let rendered = render(&n, &RenderLimits::default());
        assert_eq!(keys(&rendered), ["a", ActionId::MORE]);
        assert_eq!(rendered.overflow.len(), 1);
        assert_eq!(rendered.overflow[0].id.as_str(), "s");
    }

    #[test]
    fn replies_are_simulated_with_a_button_that_opens_the_app() {
        let n = n().with_action(Action::reply("reply", "Reply").unwrap().background());
        let rendered = render(&n, &RenderLimits::default());
        assert_eq!(rendered.actions[0].input, Input::Requested);
        assert!(rendered.actions[0].opens_app);
        let Some(Resolved::Respond(response)) = rendered.respond(&n, Some("reply"), None, true)
        else {
            panic!("reply resolves")
        };
        assert!(response.input_requested);
        assert!(response.launched_app);
        let native = RenderLimits {
            inline_reply: true,
            ..RenderLimits::default()
        };
        let rendered = render(&n, &native);
        assert_eq!(rendered.actions[0].input, Input::Reply);
        assert!(!rendered.actions[0].opens_app);
        let Some(Resolved::Respond(response)) =
            rendered.respond(&n, Some("reply"), Some("hi".to_owned()), false)
        else {
            panic!("reply resolves")
        };
        assert_eq!(response.reply.as_deref(), Some("hi"));
        assert!(!response.input_requested);
    }

    #[test]
    fn snoozes_and_urls_resolve() {
        let n = n()
            .with_url("app://n")
            .with_action(
                Action::snooze("later", "Later", std::time::Duration::from_secs(60)).unwrap(),
            )
            .with_action(
                Action::button("open", "Open")
                    .unwrap()
                    .with_url("app://open"),
            );
        let rendered = render(&n, &RenderLimits::default());
        assert_eq!(
            rendered.respond(&n, Some("later"), None, false),
            Some(Resolved::Snooze(60_000))
        );
        let Some(Resolved::Respond(tap)) = rendered.respond(&n, None, None, false) else {
            panic!()
        };
        assert_eq!(tap.url.as_deref(), Some("app://n"));
        let Some(Resolved::Respond(open)) = rendered.respond(&n, Some("open"), None, false) else {
            panic!()
        };
        assert_eq!(open.url.as_deref(), Some("app://open"));
        assert_eq!(rendered.respond(&n, Some("nope"), None, false), None);
    }

    #[test]
    fn calls_get_the_plugin_actions() {
        let call = n().with_style(Style::Call(CallInfo {
            caller: Participant::new("ana", "Ana"),
            kind: CallKind::Incoming,
            video: false,
        }));
        let rendered = render(&call, &RenderLimits::default());
        assert_eq!(
            keys(&rendered),
            [ActionId::CALL_DECLINE, ActionId::CALL_ANSWER]
        );
        let Some(Resolved::Respond(answer)) =
            rendered.respond(&call, Some(ActionId::CALL_ANSWER), None, false)
        else {
            panic!()
        };
        assert_eq!(
            answer.action,
            ResponseAction::Button(ActionId::reserved(ActionId::CALL_ANSWER))
        );
    }
}
