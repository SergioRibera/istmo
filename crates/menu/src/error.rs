use std::fmt;

use istmo_macros::message;

use crate::{ItemId, RadioGroupId};

/// Why a menu or a patch was refused. Names the node at fault.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MenuError {
    /// Two nodes share an id.
    DuplicateId(ItemId),
    /// The node's parent is not an earlier node of the menu.
    UnknownParent(ItemId),
    /// The node's parent is not a submenu.
    ParentNotSubmenu(ItemId),
    /// The node is nested deeper than [`MAX_DEPTH`](crate::MAX_DEPTH).
    TooDeep(ItemId),
    /// The menu has more than [`MAX_NODES`](crate::MAX_NODES) nodes.
    TooManyNodes(u32),
    /// A label, subtitle or badge is longer than
    /// [`MAX_LABEL_BYTES`](crate::MAX_LABEL_BYTES).
    TextTooLong(ItemId),
    /// A shortcut key is not one character or a known key name.
    BadShortcut(ItemId),
    /// Radio items of one group sit under different parents.
    RadioGroupSplit(RadioGroupId),
    /// More than one radio item of a group is selected.
    RadioGroupConflict(RadioGroupId),
    /// A patch names an item the menu does not have.
    UnknownItem(ItemId),
    /// A patch does not fit the item's kind (a check state for an
    /// action, a label for a separator).
    KindMismatch(ItemId),
}

impl fmt::Display for MenuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "item id {} is used twice", id.value),
            Self::UnknownParent(id) => write!(
                f,
                "item {} names a parent that is not an earlier item",
                id.value
            ),
            Self::ParentNotSubmenu(id) => {
                write!(f, "the parent of item {} is not a submenu", id.value)
            }
            Self::TooDeep(id) => write!(
                f,
                "item {} is nested deeper than {} levels",
                id.value,
                crate::MAX_DEPTH
            ),
            Self::TooManyNodes(count) => write!(
                f,
                "the menu has {count} items, more than {}",
                crate::MAX_NODES
            ),
            Self::TextTooLong(id) => write!(
                f,
                "a text of item {} is longer than {} bytes",
                id.value,
                crate::MAX_LABEL_BYTES
            ),
            Self::BadShortcut(id) => write!(f, "item {} has an unknown shortcut key", id.value),
            Self::RadioGroupSplit(group) => {
                write!(f, "radio group {} spans more than one submenu", group.value)
            }
            Self::RadioGroupConflict(group) => write!(
                f,
                "radio group {} has more than one selected item",
                group.value
            ),
            Self::UnknownItem(id) => write!(f, "no item {} in the menu", id.value),
            Self::KindMismatch(id) => {
                write!(
                    f,
                    "the change does not apply to the kind of item {}",
                    id.value
                )
            }
        }
    }
}

impl std::error::Error for MenuError {}
