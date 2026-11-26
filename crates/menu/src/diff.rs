use std::mem::discriminant;

use crate::{ItemChange, ItemKind, ItemPatch, MenuNode};

/// How a replacement menu differs from the current one.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MenuDiff {
    /// Nothing visible changed.
    Same,
    /// Same items in the same places: the listed properties changed.
    Patches(Vec<ItemPatch>),
    /// Items were added, removed, moved or changed kind.
    Structure,
}

impl MenuDiff {
    pub(crate) fn between(old: &[MenuNode], new: &[MenuNode]) -> Self {
        if old.len() != new.len() {
            return Self::Structure;
        }
        let mut patches = Vec::new();
        for (before, after) in old.iter().zip(new) {
            if !same_place(before, after) {
                return Self::Structure;
            }
            push_changes(before, after, &mut patches);
        }
        if patches.is_empty() {
            Self::Same
        } else {
            Self::Patches(patches)
        }
    }
}

fn same_place(before: &MenuNode, after: &MenuNode) -> bool {
    before.id == after.id
        && before.parent == after.parent
        && before.is_default == after.is_default
        && discriminant(&before.kind) == discriminant(&after.kind)
        && match (before.kind, after.kind) {
            (ItemKind::Radio(a), ItemKind::Radio(b)) => a.group == b.group,
            _ => true,
        }
}

fn push_changes(before: &MenuNode, after: &MenuNode, patches: &mut Vec<ItemPatch>) {
    let mut push = |change| {
        patches.push(ItemPatch {
            id: after.id,
            change,
        });
    };
    if before.label != after.label {
        push(ItemChange::Label(after.label.clone()));
    }
    if before.enabled != after.enabled {
        push(ItemChange::Enabled(after.enabled));
    }
    if before.visible != after.visible {
        push(ItemChange::Visible(after.visible));
    }
    match (before.kind, after.kind) {
        (ItemKind::Check(a), ItemKind::Check(b)) if a.state != b.state => {
            push(ItemChange::Check(b.state));
        }
        (ItemKind::Radio(a), ItemKind::Radio(b)) if a.selected != b.selected => {
            push(ItemChange::Selected(b.selected));
        }
        _ => {}
    }
    if before.icon != after.icon {
        push(ItemChange::Icon(after.icon.clone()));
    }
    if before.subtitle != after.subtitle {
        push(ItemChange::Subtitle(after.subtitle.clone()));
    }
    if before.badge != after.badge {
        push(ItemChange::Badge(after.badge.clone()));
    }
    if before.shortcut != after.shortcut {
        push(ItemChange::Shortcut(after.shortcut.clone()));
    }
}
