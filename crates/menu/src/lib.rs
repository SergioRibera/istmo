//! Menu model shared by the istmo menu plugins (`istmo-tray`).
//!
//! A menu travels as a flat list of [`MenuNode`]s in display order. A
//! node whose `parent` is set belongs to that submenu, which comes
//! earlier in the list. [`Menu`] is the validated form a host keeps: it
//! applies [`ItemPatch`]es, runs the automatic check / radio toggling
//! when an item is activated, and diffs itself against a replacement.
//!
//! `istmo-build` generates these types once per app (Kotlin package
//! `dev.istmo.menu`, Swift namespace `IstmoMenu`) for every plugin
//! that uses them.

#![doc(html_root_url = "https://docs.rs/istmo-menu")]

use istmo_core::NativeHandleId;
use istmo_macros::message;

mod builder;
mod diff;
mod error;
mod label;
mod menu;
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub(crate)` marks crate-internal items; plain `pub` in private modules trips `unreachable_pub`"
)]
mod validate;

pub use builder::MenuBuilder;
pub use diff::MenuDiff;
pub use error::MenuError;
pub use label::Label;
pub use menu::{Activation, ActivationEffect, Menu};
pub use validate::{MAX_DEPTH, MAX_LABEL_BYTES, MAX_NODES};

/// Identifier of a menu item, chosen by the app. Unique within a menu.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId {
    pub value: u32,
}

impl From<u32> for ItemId {
    fn from(value: u32) -> Self {
        Self { value }
    }
}

/// Identifier of a group of radio items: selecting one clears the
/// others of the same group.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RadioGroupId {
    pub value: u32,
}

impl From<u32> for RadioGroupId {
    fn from(value: u32) -> Self {
        Self { value }
    }
}

/// One entry of a menu.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuNode {
    pub id: ItemId,
    /// The submenu this node belongs to; `None` for the top level.
    pub parent: Option<ItemId>,
    pub kind: ItemKind,
    /// Text shown. `&` marks the next character as the mnemonic, `&&`
    /// is a literal `&` (see [`Label`]).
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
    pub icon: Option<IconRef>,
    /// Keyboard shortcut shown next to the label. Display only.
    pub shortcut: Option<Shortcut>,
    /// Second line under the label.
    pub subtitle: Option<String>,
    /// Short text drawn at the trailing edge (a count, "New").
    pub badge: Option<String>,
    /// The menu's default item: drawn bold where the platform does, run
    /// on a double click of a tray icon.
    pub is_default: bool,
}

/// What a [`MenuNode`] is.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ItemKind {
    /// A plain clickable item.
    Action,
    Check(CheckItem),
    Radio(RadioItem),
    /// Opens the nodes whose `parent` is this node.
    Submenu,
    Separator,
    /// A section title: shown, never clickable.
    Header,
}

/// A check item's state and behaviour.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckItem {
    pub state: CheckState,
    /// Flip the state when the item is activated, before the app hears
    /// about it. Off: the app owns the state and patches it.
    pub auto_toggle: bool,
}

/// State of a check item.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CheckState {
    Off,
    On,
    /// Neither: some of what the item stands for is on.
    Mixed,
}

impl CheckState {
    /// The state a click leads to: `Off` → `On`, `On` → `Off`,
    /// `Mixed` → `On`.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Off | Self::Mixed => Self::On,
            Self::On => Self::Off,
        }
    }
}

/// A radio item's group and state.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RadioItem {
    pub group: RadioGroupId,
    pub selected: bool,
    /// Select the item (and clear the rest of its group) when it is
    /// activated, before the app hears about it.
    pub auto_select: bool,
}

/// A key combination shown next to an item.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub modifiers: Modifiers,
    /// One character (`"S"`, `","`) or a key name: `F1`–`F24`, `Enter`,
    /// `Escape`, `Tab`, `Space`, `Backspace`, `Delete`, `Insert`, `Home`,
    /// `End`, `PageUp`, `PageDown`, `Up`, `Down`, `Left`, `Right`.
    pub key: String,
}

/// Modifier keys of a [`Shortcut`], as bits.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    pub bits: u8,
}

impl Modifiers {
    pub const NONE: Self = Self { bits: 0 };
    pub const SHIFT: Self = Self { bits: 1 };
    pub const CONTROL: Self = Self { bits: 1 << 1 };
    pub const ALT: Self = Self { bits: 1 << 2 };
    /// Command on macOS, the Windows / Super key elsewhere.
    pub const SUPER: Self = Self { bits: 1 << 3 };

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.bits & other.bits == other.bits
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// One change to one item.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub struct ItemPatch {
    pub id: ItemId,
    pub change: ItemChange,
}

/// A property of an item that can change without rebuilding the menu.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ItemChange {
    Label(String),
    Enabled(bool),
    Visible(bool),
    /// New state of a check item.
    Check(CheckState),
    /// Select (`true`) or clear a radio item. Selecting clears the rest
    /// of its group.
    Selected(bool),
    Icon(Option<IconRef>),
    Subtitle(Option<String>),
    Badge(Option<String>),
    Shortcut(Option<Shortcut>),
}

/// One encoding of a picture.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageData {
    Svg(Vec<u8>),
    Png(Vec<u8>),
    /// No transparency: shows as an opaque rectangle.
    Jpeg(Vec<u8>),
    /// A platform icon name: an SF Symbol on macOS, an icon theme name
    /// on Linux. Skipped where names are not supported.
    PlatformName(String),
}

impl std::fmt::Debug for ImageData {
    // Payloads are image files: their bytes say nothing in a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Svg(bytes) => write!(f, "Svg({} bytes)", bytes.len()),
            Self::Png(bytes) => write!(f, "Png({} bytes)", bytes.len()),
            Self::Jpeg(bytes) => write!(f, "Jpeg({} bytes)", bytes.len()),
            Self::PlatformName(name) => f.debug_tuple("PlatformName").field(name).finish(),
        }
    }
}

/// A picture in one or more representations (an SVG, PNGs of several
/// sizes, a platform name with a raster fallback).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IconImage {
    pub reps: Vec<ImageData>,
    /// A macOS template image: only its alpha is used and the system
    /// tints it for the menu bar's appearance. Ignored elsewhere.
    pub template: bool,
}

impl IconImage {
    /// One representation, not a template.
    #[must_use]
    pub fn new(rep: ImageData) -> Self {
        Self {
            reps: vec![rep],
            template: false,
        }
    }

    /// The same picture as a macOS template image.
    #[must_use]
    pub const fn template(mut self) -> Self {
        self.template = true;
        self
    }

    /// Adds a representation, tried after the ones already listed.
    #[must_use]
    pub fn or(mut self, rep: ImageData) -> Self {
        self.reps.push(rep);
        self
    }
}

/// An icon, given inline or by the handle of an icon registered with
/// the plugin (decoded once, cheap to reuse).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IconRef {
    Inline(IconImage),
    Registered(NativeHandleId),
}

impl From<IconImage> for IconRef {
    fn from(image: IconImage) -> Self {
        Self::Inline(image)
    }
}
