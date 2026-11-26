use crate::validate::{Index, check_change};
use crate::{
    CheckState, ItemChange, ItemId, ItemKind, ItemPatch, MenuDiff, MenuError, MenuNode,
    RadioGroupId,
};

/// A validated menu: the state a host keeps and updates.
#[derive(Debug, Clone, Default)]
pub struct Menu {
    nodes: Vec<MenuNode>,
    index: Index,
}

/// What activating an item did to the menu.
#[derive(Debug, Clone, PartialEq)]
pub struct Activation {
    pub item: ItemId,
    pub effect: ActivationEffect,
    /// The changes made to the menu (already applied), for the
    /// platform's view of it.
    pub patches: Vec<ItemPatch>,
}

/// The automatic state change an activation made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ActivationEffect {
    /// Nothing: an action, or toggling is off.
    None,
    /// A check item's new state.
    Checked(CheckState),
    /// A radio item became the selected one of its group.
    Selected(RadioGroupId),
}

impl TryFrom<Vec<MenuNode>> for Menu {
    type Error = MenuError;

    fn try_from(nodes: Vec<MenuNode>) -> Result<Self, MenuError> {
        let index = Index::build(&nodes)?;
        Ok(Self { nodes, index })
    }
}

impl Menu {
    /// Every node, in display order.
    #[must_use]
    pub fn nodes(&self) -> &[MenuNode] {
        &self.nodes
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    #[must_use]
    pub fn get(&self, id: ItemId) -> Option<&MenuNode> {
        self.index.by_id.get(&id).map(|&at| &self.nodes[at])
    }

    /// The nodes directly under `parent` (`None`: the top level), in
    /// display order.
    pub fn children(&self, parent: Option<ItemId>) -> impl Iterator<Item = &MenuNode> {
        self.index
            .children
            .get(&parent)
            .into_iter()
            .flatten()
            .map(|&at| &self.nodes[at])
    }

    /// Applies `patches` in order, all or none. Returns the changes
    /// made, which add the radio items a selection cleared.
    ///
    /// # Errors
    /// The first patch naming an unknown item or not fitting its item;
    /// the menu is left unchanged.
    pub fn apply(&mut self, patches: &[ItemPatch]) -> Result<Vec<ItemPatch>, MenuError> {
        for patch in patches {
            let node = self.get(patch.id).ok_or(MenuError::UnknownItem(patch.id))?;
            check_change(patch.id, node.kind, &patch.change)?;
        }
        let mut applied = Vec::with_capacity(patches.len());
        for patch in patches {
            self.apply_one(patch, &mut applied);
        }
        Ok(applied)
    }

    /// Runs what activating `id` does to the menu: flips a check item,
    /// selects a radio item. `None` when the item is unknown, disabled,
    /// hidden or not clickable (separator, header, submenu).
    pub fn activate(&mut self, id: ItemId) -> Option<Activation> {
        let node = self.get(id)?;
        if !node.enabled || !node.visible {
            return None;
        }
        let (effect, patch) = match node.kind {
            ItemKind::Check(check) if check.auto_toggle => {
                let state = check.state.toggled();
                (
                    ActivationEffect::Checked(state),
                    Some(ItemChange::Check(state)),
                )
            }
            ItemKind::Radio(radio) if radio.auto_select => (
                ActivationEffect::Selected(radio.group),
                (!radio.selected).then_some(ItemChange::Selected(true)),
            ),
            ItemKind::Action | ItemKind::Check(_) | ItemKind::Radio(_) => {
                (ActivationEffect::None, None)
            }
            ItemKind::Submenu | ItemKind::Separator | ItemKind::Header => return None,
        };
        let mut patches = Vec::new();
        if let Some(change) = patch {
            self.apply_one(&ItemPatch { id, change }, &mut patches);
        }
        Some(Activation {
            item: id,
            effect,
            patches,
        })
    }

    /// How `next` differs from this menu.
    #[must_use]
    pub fn diff(&self, next: &Self) -> MenuDiff {
        MenuDiff::between(&self.nodes, &next.nodes)
    }

    #[must_use]
    pub fn into_nodes(self) -> Vec<MenuNode> {
        self.nodes
    }

    /// Applies a checked patch, recording it and the radio items it
    /// cleared.
    fn apply_one(&mut self, patch: &ItemPatch, applied: &mut Vec<ItemPatch>) {
        let Some(&at) = self.index.by_id.get(&patch.id) else {
            return;
        };
        if patch.change == ItemChange::Selected(true)
            && let ItemKind::Radio(radio) = self.nodes[at].kind
        {
            self.clear_group(radio.group, patch.id, applied);
        }
        let node = &mut self.nodes[at];
        match &patch.change {
            ItemChange::Label(text) => node.label.clone_from(text),
            ItemChange::Enabled(on) => node.enabled = *on,
            ItemChange::Visible(on) => node.visible = *on,
            ItemChange::Check(state) => {
                if let ItemKind::Check(check) = &mut node.kind {
                    check.state = *state;
                }
            }
            ItemChange::Selected(on) => {
                if let ItemKind::Radio(radio) = &mut node.kind {
                    radio.selected = *on;
                }
            }
            ItemChange::Icon(icon) => node.icon.clone_from(icon),
            ItemChange::Subtitle(text) => node.subtitle.clone_from(text),
            ItemChange::Badge(text) => node.badge.clone_from(text),
            ItemChange::Shortcut(shortcut) => node.shortcut.clone_from(shortcut),
        }
        applied.push(patch.clone());
    }

    fn clear_group(&mut self, group: RadioGroupId, keep: ItemId, applied: &mut Vec<ItemPatch>) {
        for node in &mut self.nodes {
            if node.id == keep {
                continue;
            }
            if let ItemKind::Radio(radio) = &mut node.kind
                && radio.group == group
                && radio.selected
            {
                radio.selected = false;
                applied.push(ItemPatch {
                    id: node.id,
                    change: ItemChange::Selected(false),
                });
            }
        }
    }
}
