use crate::{
    CheckItem, CheckState, IconRef, ItemId, ItemKind, Menu, MenuError, MenuNode, RadioGroupId,
    RadioItem, Shortcut,
};

/// Builds a menu's node list in display order.
///
/// Item methods add a node; the modifier methods (`disabled`, `icon`,
/// `shortcut`, …) change the node added last at the same level (after
/// `submenu`, the submenu itself).
///
/// ```
/// use istmo_menu::{CheckState, MenuBuilder, Modifiers, RadioGroupId, Shortcut};
///
/// let theme = RadioGroupId::from(1);
/// let menu = MenuBuilder::default()
///     .action(1, "&Open")
///     .shortcut(Shortcut { modifiers: Modifiers::CONTROL, key: "O".into() })
///     .check(2, "Start at login", CheckState::Off)
///     .separator(3)
///     .submenu(4, "Theme", |m| {
///         m.radio(5, "Light", theme, true).radio(6, "Dark", theme, false)
///     })
///     .action(7, "&Quit")
///     .build()
///     .expect("invariant: ids are unique");
/// assert_eq!(menu.children(None).count(), 5);
/// ```
#[derive(Debug, Clone, Default)]
#[must_use]
pub struct MenuBuilder {
    nodes: Vec<MenuNode>,
    parent: Option<ItemId>,
    /// The node the modifiers change: the last one added at this level.
    last: Option<usize>,
}

impl MenuNode {
    /// An enabled, visible node with no icon, shortcut or extra text.
    pub fn new(id: impl Into<ItemId>, kind: ItemKind, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            parent: None,
            kind,
            label: label.into(),
            enabled: true,
            visible: true,
            icon: None,
            shortcut: None,
            subtitle: None,
            badge: None,
            is_default: false,
        }
    }
}

impl MenuBuilder {
    pub fn action(self, id: impl Into<ItemId>, label: impl Into<String>) -> Self {
        self.push(MenuNode::new(id, ItemKind::Action, label))
    }

    /// A check item that toggles itself when activated.
    pub fn check(self, id: impl Into<ItemId>, label: impl Into<String>, state: CheckState) -> Self {
        let kind = ItemKind::Check(CheckItem {
            state,
            auto_toggle: true,
        });
        self.push(MenuNode::new(id, kind, label))
    }

    /// A radio item that selects itself when activated.
    pub fn radio(
        self,
        id: impl Into<ItemId>,
        label: impl Into<String>,
        group: RadioGroupId,
        selected: bool,
    ) -> Self {
        let kind = ItemKind::Radio(RadioItem {
            group,
            selected,
            auto_select: true,
        });
        self.push(MenuNode::new(id, kind, label))
    }

    pub fn separator(self, id: impl Into<ItemId>) -> Self {
        self.push(MenuNode::new(id, ItemKind::Separator, String::new()))
    }

    pub fn header(self, id: impl Into<ItemId>, label: impl Into<String>) -> Self {
        self.push(MenuNode::new(id, ItemKind::Header, label))
    }

    /// A submenu whose items `items` adds.
    pub fn submenu(
        self,
        id: impl Into<ItemId>,
        label: impl Into<String>,
        items: impl FnOnce(Self) -> Self,
    ) -> Self {
        let id = id.into();
        let outer = self.parent;
        let mut inner = self.push(MenuNode::new(id, ItemKind::Submenu, label));
        let submenu = inner.last;
        inner.parent = Some(id);
        let mut built = items(inner);
        built.parent = outer;
        built.last = submenu;
        built
    }

    pub fn disabled(self) -> Self {
        self.edit(|node| node.enabled = false)
    }

    pub fn hidden(self) -> Self {
        self.edit(|node| node.visible = false)
    }

    pub fn icon(self, icon: impl Into<IconRef>) -> Self {
        let icon = icon.into();
        self.edit(|node| node.icon = Some(icon))
    }

    pub fn shortcut(self, shortcut: Shortcut) -> Self {
        self.edit(|node| node.shortcut = Some(shortcut))
    }

    pub fn subtitle(self, text: impl Into<String>) -> Self {
        let text = text.into();
        self.edit(|node| node.subtitle = Some(text))
    }

    pub fn badge(self, text: impl Into<String>) -> Self {
        let text = text.into();
        self.edit(|node| node.badge = Some(text))
    }

    pub fn default_item(self) -> Self {
        self.edit(|node| node.is_default = true)
    }

    /// The app owns the state of the last check / radio item: activating
    /// it changes nothing until the app sends a patch.
    pub fn manual(self) -> Self {
        self.edit(|node| match &mut node.kind {
            ItemKind::Check(check) => check.auto_toggle = false,
            ItemKind::Radio(radio) => radio.auto_select = false,
            ItemKind::Action | ItemKind::Submenu | ItemKind::Separator | ItemKind::Header => {}
        })
    }

    /// The node list, unvalidated.
    #[must_use]
    pub fn into_nodes(self) -> Vec<MenuNode> {
        self.nodes
    }

    /// The validated menu.
    ///
    /// # Errors
    /// See [`MenuError`].
    pub fn build(self) -> Result<Menu, MenuError> {
        Menu::try_from(self.nodes)
    }

    fn push(mut self, mut node: MenuNode) -> Self {
        node.parent = self.parent;
        self.last = Some(self.nodes.len());
        self.nodes.push(node);
        self
    }

    fn edit(mut self, change: impl FnOnce(&mut MenuNode)) -> Self {
        if let Some(node) = self.last.and_then(|at| self.nodes.get_mut(at)) {
            change(node);
        }
        self
    }
}
