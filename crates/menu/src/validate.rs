use std::collections::HashMap;

use crate::{ItemChange, ItemId, ItemKind, MenuError, MenuNode, RadioGroupId, Shortcut};

/// Most nodes a menu may have.
pub const MAX_NODES: usize = 4096;

/// Deepest submenu nesting: a top-level node is at depth 0.
pub const MAX_DEPTH: usize = 16;

/// Longest label, subtitle or badge, in bytes.
pub const MAX_LABEL_BYTES: usize = 1024;

const KEY_NAMES: &[&str] = &[
    "Enter",
    "Escape",
    "Tab",
    "Space",
    "Backspace",
    "Delete",
    "Insert",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Up",
    "Down",
    "Left",
    "Right",
];

/// Positions of the nodes of a valid menu.
#[derive(Debug, Clone, Default)]
pub(super) struct Index {
    pub(super) by_id: HashMap<ItemId, usize>,
    /// Node positions per parent (`None` = top level), in display order.
    pub(super) children: HashMap<Option<ItemId>, Vec<usize>>,
}

impl Index {
    pub(super) fn build(nodes: &[MenuNode]) -> Result<Self, MenuError> {
        if nodes.len() > MAX_NODES {
            return Err(MenuError::TooManyNodes(
                u32::try_from(nodes.len()).unwrap_or(u32::MAX),
            ));
        }
        let mut index = Self::default();
        let mut depths: Vec<usize> = Vec::with_capacity(nodes.len());
        let mut groups: HashMap<RadioGroupId, (Option<ItemId>, bool)> = HashMap::new();
        for (at, node) in nodes.iter().enumerate() {
            check_texts(node)?;
            let level = match node.parent {
                None => 0,
                Some(parent) => {
                    let parent_at = *index
                        .by_id
                        .get(&parent)
                        .ok_or(MenuError::UnknownParent(node.id))?;
                    if !matches!(nodes[parent_at].kind, ItemKind::Submenu) {
                        return Err(MenuError::ParentNotSubmenu(node.id));
                    }
                    depths[parent_at] + 1
                }
            };
            if level >= MAX_DEPTH {
                return Err(MenuError::TooDeep(node.id));
            }
            if index.by_id.insert(node.id, at).is_some() {
                return Err(MenuError::DuplicateId(node.id));
            }
            depths.push(level);
            index.children.entry(node.parent).or_default().push(at);
            if let ItemKind::Radio(radio) = node.kind {
                let (parent, selected) = groups.entry(radio.group).or_insert((node.parent, false));
                if *parent != node.parent {
                    return Err(MenuError::RadioGroupSplit(radio.group));
                }
                if radio.selected {
                    if *selected {
                        return Err(MenuError::RadioGroupConflict(radio.group));
                    }
                    *selected = true;
                }
            }
        }
        Ok(index)
    }
}

fn check_texts(node: &MenuNode) -> Result<(), MenuError> {
    let texts = [
        Some(node.label.as_str()),
        node.subtitle.as_deref(),
        node.badge.as_deref(),
    ];
    if texts
        .into_iter()
        .flatten()
        .any(|t| t.len() > MAX_LABEL_BYTES)
    {
        return Err(MenuError::TextTooLong(node.id));
    }
    if node.shortcut.as_ref().is_some_and(|s| !is_valid_key(s)) {
        return Err(MenuError::BadShortcut(node.id));
    }
    Ok(())
}

fn is_valid_key(shortcut: &Shortcut) -> bool {
    let key = shortcut.key.as_str();
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return !c.is_control() && !c.is_whitespace();
    }
    KEY_NAMES.contains(&key)
        || key
            .strip_prefix('F')
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=24).contains(&n) && !key.starts_with("F0"))
}

/// Whether `change` may be applied to a node of `kind`, and its values
/// are in bounds.
pub(super) fn check_change(
    id: ItemId,
    kind: ItemKind,
    change: &ItemChange,
) -> Result<(), MenuError> {
    let fits = match change {
        ItemChange::Enabled(_) | ItemChange::Visible(_) => true,
        ItemChange::Check(_) => matches!(kind, ItemKind::Check(_)),
        ItemChange::Selected(_) => matches!(kind, ItemKind::Radio(_)),
        ItemChange::Label(_)
        | ItemChange::Icon(_)
        | ItemChange::Subtitle(_)
        | ItemChange::Badge(_)
        | ItemChange::Shortcut(_) => !matches!(kind, ItemKind::Separator),
    };
    if !fits {
        return Err(MenuError::KindMismatch(id));
    }
    let too_long = match change {
        ItemChange::Label(text) => text.len() > MAX_LABEL_BYTES,
        ItemChange::Subtitle(text) | ItemChange::Badge(text) => {
            text.as_ref().is_some_and(|t| t.len() > MAX_LABEL_BYTES)
        }
        ItemChange::Enabled(_)
        | ItemChange::Visible(_)
        | ItemChange::Check(_)
        | ItemChange::Selected(_)
        | ItemChange::Icon(_)
        | ItemChange::Shortcut(_) => false,
    };
    if too_long {
        return Err(MenuError::TextTooLong(id));
    }
    if let ItemChange::Shortcut(Some(shortcut)) = change
        && !is_valid_key(shortcut)
    {
        return Err(MenuError::BadShortcut(id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Modifiers;

    fn key(key: &str) -> Shortcut {
        Shortcut {
            modifiers: Modifiers::CONTROL,
            key: key.to_owned(),
        }
    }

    #[test]
    fn shortcut_keys() {
        for good in ["S", ",", "ñ", "F1", "F24", "PageUp", "Escape"] {
            assert!(is_valid_key(&key(good)), "{good}");
        }
        for bad in ["", " ", "\n", "F0", "F25", "F01", "Ctrl", "SS"] {
            assert!(!is_valid_key(&key(bad)), "{bad}");
        }
    }
}
