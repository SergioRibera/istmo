use istmo_menu::{
    ActivationEffect, CheckState, ItemChange, ItemId, ItemKind, ItemPatch, Menu, MenuBuilder,
    MenuDiff, MenuError, MenuNode, RadioGroupId,
};

const THEME: RadioGroupId = RadioGroupId { value: 7 };

const fn id(value: u32) -> ItemId {
    ItemId { value }
}

fn sample() -> MenuBuilder {
    MenuBuilder::default()
        .action(1, "&Open")
        .check(2, "Start at login", CheckState::Off)
        .separator(3)
        .submenu(4, "Theme", |m| {
            m.radio(5, "Light", THEME, true)
                .radio(6, "Dark", THEME, false)
                .radio(8, "System", THEME, false)
        })
        .disabled()
        .action(9, "Quit")
}

const fn patch(value: u32, change: ItemChange) -> ItemPatch {
    ItemPatch {
        id: id(value),
        change,
    }
}

#[test]
fn builder_nests_submenus_and_modifies_the_submenu_itself() {
    let menu = sample().build().unwrap();
    let top: Vec<_> = menu.children(None).map(|n| n.id.value).collect();
    assert_eq!(top, [1, 2, 3, 4, 9]);
    let theme: Vec<_> = menu.children(Some(id(4))).map(|n| n.id.value).collect();
    assert_eq!(theme, [5, 6, 8]);
    assert!(!menu.get(id(4)).unwrap().enabled);
    assert!(menu.get(id(8)).unwrap().enabled);
}

#[test]
fn invalid_menus_name_the_node() {
    let dup = MenuBuilder::default().action(1, "a").action(1, "b").build();
    assert_eq!(dup.unwrap_err(), MenuError::DuplicateId(id(1)));

    let mut orphan = MenuNode::new(2, ItemKind::Action, "x");
    orphan.parent = Some(id(9));
    assert_eq!(
        Menu::try_from(vec![orphan]).unwrap_err(),
        MenuError::UnknownParent(id(2))
    );

    let mut under_action = MenuNode::new(2, ItemKind::Action, "x");
    under_action.parent = Some(id(1));
    let nodes = vec![MenuNode::new(1, ItemKind::Action, "a"), under_action];
    assert_eq!(
        Menu::try_from(nodes).unwrap_err(),
        MenuError::ParentNotSubmenu(id(2))
    );

    let conflict = MenuBuilder::default()
        .radio(1, "a", THEME, true)
        .radio(2, "b", THEME, true)
        .build();
    assert_eq!(conflict.unwrap_err(), MenuError::RadioGroupConflict(THEME));

    let split = MenuBuilder::default()
        .radio(1, "a", THEME, false)
        .submenu(2, "s", |m| m.radio(3, "b", THEME, false))
        .build();
    assert_eq!(split.unwrap_err(), MenuError::RadioGroupSplit(THEME));

    let long = MenuBuilder::default().action(1, "x".repeat(2000)).build();
    assert_eq!(long.unwrap_err(), MenuError::TextTooLong(id(1)));
}

#[test]
fn nesting_deeper_than_the_limit_is_refused() {
    fn nest(builder: MenuBuilder, level: u32) -> MenuBuilder {
        if level == 20 {
            return builder.action(1000, "leaf");
        }
        builder.submenu(level, "s", |m| nest(m, level + 1))
    }
    let err = nest(MenuBuilder::default(), 0).build().unwrap_err();
    assert_eq!(err, MenuError::TooDeep(id(16)));
}

#[test]
fn activation_toggles_checks_and_selects_radios() {
    let mut menu = sample().build().unwrap();

    let check = menu.activate(id(2)).unwrap();
    assert_eq!(check.effect, ActivationEffect::Checked(CheckState::On));
    assert_eq!(check.patches, [patch(2, ItemChange::Check(CheckState::On))]);

    let radio = menu.activate(id(6)).unwrap();
    assert_eq!(radio.effect, ActivationEffect::Selected(THEME));
    assert_eq!(
        radio.patches,
        [
            patch(5, ItemChange::Selected(false)),
            patch(6, ItemChange::Selected(true)),
        ]
    );

    // Selecting the selected radio changes nothing.
    assert_eq!(menu.activate(id(6)).unwrap().patches, []);
    // Separators, submenus and unknown ids are not activatable.
    assert!(menu.activate(id(3)).is_none());
    assert!(menu.activate(id(4)).is_none());
    assert!(menu.activate(id(99)).is_none());
}

#[test]
fn manual_items_leave_the_state_to_the_app() {
    let mut menu = MenuBuilder::default()
        .check(1, "c", CheckState::Mixed)
        .manual()
        .build()
        .unwrap();
    let activation = menu.activate(id(1)).unwrap();
    assert_eq!(activation.effect, ActivationEffect::None);
    assert!(activation.patches.is_empty());
}

#[test]
fn disabled_items_are_not_activated() {
    let mut menu = MenuBuilder::default()
        .action(1, "a")
        .disabled()
        .build()
        .unwrap();
    assert!(menu.activate(id(1)).is_none());
}

#[test]
fn patches_apply_in_order_and_atomically() {
    let mut menu = sample().build().unwrap();
    let applied = menu
        .apply(&[
            patch(1, ItemChange::Label("Open…".into())),
            patch(8, ItemChange::Selected(true)),
        ])
        .unwrap();
    assert_eq!(
        applied,
        [
            patch(1, ItemChange::Label("Open…".into())),
            patch(5, ItemChange::Selected(false)),
            patch(8, ItemChange::Selected(true)),
        ]
    );

    let before = menu.nodes().to_vec();
    let err = menu
        .apply(&[
            patch(1, ItemChange::Label("changed".into())),
            patch(3, ItemChange::Label("separator".into())),
        ])
        .unwrap_err();
    assert_eq!(err, MenuError::KindMismatch(id(3)));
    assert_eq!(menu.nodes(), before);

    assert_eq!(
        menu.apply(&[patch(42, ItemChange::Enabled(false))]),
        Err(MenuError::UnknownItem(id(42)))
    );
    assert_eq!(
        menu.apply(&[patch(1, ItemChange::Check(CheckState::On))]),
        Err(MenuError::KindMismatch(id(1)))
    );
}

#[test]
fn diff_reports_property_changes_as_patches() {
    let old = sample().build().unwrap();
    let new = MenuBuilder::default()
        .action(1, "&Open file")
        .check(2, "Start at login", CheckState::On)
        .separator(3)
        .submenu(4, "Theme", |m| {
            m.radio(5, "Light", THEME, false)
                .radio(6, "Dark", THEME, true)
                .radio(8, "System", THEME, false)
        })
        .disabled()
        .action(9, "Quit")
        .badge("1")
        .build()
        .unwrap();
    let MenuDiff::Patches(patches) = old.diff(&new) else {
        panic!("expected patches");
    };
    assert_eq!(
        patches,
        [
            patch(1, ItemChange::Label("&Open file".into())),
            patch(2, ItemChange::Check(CheckState::On)),
            patch(5, ItemChange::Selected(false)),
            patch(6, ItemChange::Selected(true)),
            patch(9, ItemChange::Badge(Some("1".into()))),
        ]
    );
    assert_eq!(old.diff(&old.clone()), MenuDiff::Same);
}

#[test]
fn diff_reports_moves_and_kind_changes_as_structure() {
    let old = sample().build().unwrap();
    let moved = MenuBuilder::default()
        .check(2, "Start at login", CheckState::Off)
        .action(1, "&Open")
        .build()
        .unwrap();
    assert_eq!(old.diff(&moved), MenuDiff::Structure);

    let base = MenuBuilder::default().action(1, "a").build().unwrap();
    let kind = MenuBuilder::default()
        .check(1, "a", CheckState::Off)
        .build()
        .unwrap();
    assert_eq!(base.diff(&kind), MenuDiff::Structure);
}

#[test]
fn diff_patches_rebuild_the_new_menu() {
    let mut old = sample().build().unwrap();
    let new = MenuBuilder::default()
        .action(1, "Open")
        .disabled()
        .check(2, "Start at login", CheckState::Mixed)
        .separator(3)
        .submenu(4, "Theme", |m| {
            m.radio(5, "Light", THEME, false)
                .radio(6, "Dark", THEME, false)
                .radio(8, "System", THEME, true)
        })
        .action(9, "Quit")
        .build()
        .unwrap();
    let MenuDiff::Patches(patches) = old.diff(&new) else {
        panic!("expected patches");
    };
    old.apply(&patches).unwrap();
    assert_eq!(old.nodes(), new.nodes());
}
