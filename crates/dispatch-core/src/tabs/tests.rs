//! Tests for the tab model.

use super::*;

/// `count` fresh pane ids.
fn panes(count: usize) -> Vec<PaneId> {
    (0..count).map(|_| PaneId::new()).collect()
}

/// Each tab's panes, in row order.
fn layout(tabs: &ProjectTabs) -> Vec<Vec<PaneId>> {
    tabs.tabs().iter().map(|tab| tab.panes.clone()).collect()
}

/// Tabs holding `groups`, one tab per group, made the way a user makes them:
/// each group's first pane opens a new tab, the rest go into it.
fn tabs_of(groups: &[&[PaneId]]) -> ProjectTabs {
    let mut tabs = ProjectTabs::new();
    for group in groups {
        let mut current = None;
        for pane in *group {
            let place = match current {
                None => Placement::NewAfter {
                    tab: tabs.tabs().last().map(|tab| tab.id),
                },
                Some(tab) => Placement::Into { tab },
            };
            current = Some(tabs.place(*pane, place));
        }
    }
    tabs
}

#[test]
fn auto_fills_the_last_tab_then_starts_another() {
    let ids = panes(5);
    let mut tabs = ProjectTabs::new();
    for id in &ids {
        tabs.place(*id, Placement::Auto);
    }

    assert_eq!(layout(&tabs), vec![ids[..4].to_vec(), ids[4..].to_vec()]);
}

#[test]
fn auto_never_back_fills_an_earlier_tab() {
    // An older client groups panes four at a time, in order. Back-filling
    // would put a new pane on a tab that client draws it nowhere near.
    let ids = panes(6);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..5]]);

    tabs.place(ids[5], Placement::Auto);

    assert_eq!(
        layout(&tabs),
        vec![ids[..1].to_vec(), ids[1..5].to_vec(), vec![ids[5]]]
    );
}

#[test]
fn a_pane_asked_into_a_tab_with_room_goes_there() {
    let ids = panes(3);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..2]]);
    let first = tabs.tabs()[0].id;

    assert_eq!(tabs.place(ids[2], Placement::Into { tab: first }), first);
    assert_eq!(layout(&tabs), vec![vec![ids[0], ids[2]], vec![ids[1]]]);
}

#[test]
fn a_pane_asked_into_a_full_tab_starts_a_new_one_straight_after_it() {
    let ids = panes(6);
    let mut tabs = tabs_of(&[&ids[..4], &ids[4..5]]);
    let first = tabs.tabs()[0].id;

    tabs.place(ids[5], Placement::Into { tab: first });

    assert_eq!(
        layout(&tabs),
        vec![ids[..4].to_vec(), vec![ids[5]], vec![ids[4]]]
    );
}

#[test]
fn a_new_tab_goes_straight_after_the_one_named_or_at_the_end() {
    let ids = panes(4);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..2]]);
    let first = tabs.tabs()[0].id;

    tabs.place(ids[2], Placement::NewAfter { tab: Some(first) });
    tabs.place(ids[3], Placement::NewAfter { tab: None });

    assert_eq!(
        layout(&tabs),
        vec![vec![ids[0]], vec![ids[2]], vec![ids[1]], vec![ids[3]]]
    );
}

#[test]
fn a_tab_that_is_gone_gives_way_rather_than_losing_the_pane() {
    // Another client can remove a tab between this one reading it and the
    // spawn arriving; the pane has started either way and must go somewhere.
    let ids = panes(3);
    let mut tabs = tabs_of(&[&ids[..1]]);
    let gone = TabId::new();

    tabs.place(ids[1], Placement::Into { tab: gone });
    tabs.place(ids[2], Placement::NewAfter { tab: Some(gone) });

    assert_eq!(layout(&tabs), vec![vec![ids[0], ids[1]], vec![ids[2]]]);
}

#[test]
fn an_unknown_placement_is_auto() {
    let ids = panes(2);
    let mut tabs = tabs_of(&[&ids[..1]]);

    tabs.place(ids[1], Placement::Unknown);

    assert_eq!(layout(&tabs), vec![ids.clone()]);
}

#[test]
fn placing_a_pane_twice_leaves_it_where_it_is() {
    let ids = panes(2);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..]]);
    let first = tabs.tabs()[0].id;

    assert_eq!(tabs.place(ids[0], Placement::NewAfter { tab: None }), first);
    assert_eq!(layout(&tabs), vec![vec![ids[0]], vec![ids[1]]]);
}

#[test]
fn a_pane_leaving_moves_nothing_on_other_tabs() {
    let ids = panes(3);
    let mut tabs = tabs_of(&[&ids[..2], &ids[2..]]);

    assert!(tabs.remove(ids[0]));
    assert_eq!(layout(&tabs), vec![vec![ids[1]], vec![ids[2]]]);
}

#[test]
fn a_tab_with_nothing_left_on_it_is_removed() {
    let ids = panes(2);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..]]);

    tabs.remove(ids[0]);

    assert_eq!(layout(&tabs), vec![vec![ids[1]]]);
}

#[test]
fn removing_a_pane_on_no_tab_changes_nothing() {
    let ids = panes(1);
    let mut tabs = tabs_of(&[&ids[..]]);

    assert!(!tabs.remove(PaneId::new()));
    assert_eq!(layout(&tabs), vec![ids]);
}

#[test]
fn a_pane_moves_into_a_tab_with_room() {
    let ids = panes(3);
    let mut tabs = tabs_of(&[&ids[..2], &ids[2..]]);
    let second = tabs.tabs()[1].id;

    assert_eq!(
        tabs.move_pane(ids[0], Placement::Into { tab: second }),
        Ok(second)
    );
    assert_eq!(layout(&tabs), vec![vec![ids[1]], vec![ids[2], ids[0]]]);
}

#[test]
fn moving_into_a_full_tab_is_refused_and_moves_nothing() {
    let ids = panes(5);
    let mut tabs = tabs_of(&[&ids[..4], &ids[4..]]);
    let first = tabs.tabs()[0].id;

    assert_eq!(
        tabs.move_pane(ids[4], Placement::Into { tab: first }),
        Err(TabError::Full)
    );
    assert_eq!(layout(&tabs), vec![ids[..4].to_vec(), vec![ids[4]]]);
}

#[test]
fn moving_into_a_tab_that_is_gone_is_refused() {
    let ids = panes(1);
    let mut tabs = tabs_of(&[&ids[..]]);

    assert_eq!(
        tabs.move_pane(ids[0], Placement::Into { tab: TabId::new() }),
        Err(TabError::NoSuchTab)
    );
    assert_eq!(
        tabs.move_pane(
            ids[0],
            Placement::NewAfter {
                tab: Some(TabId::new())
            }
        ),
        Err(TabError::NoSuchTab)
    );
}

#[test]
fn a_pane_on_no_tab_cannot_be_moved() {
    let mut tabs = ProjectTabs::new();

    assert_eq!(
        tabs.move_pane(PaneId::new(), Placement::Auto),
        Err(TabError::NoSuchPane)
    );
}

#[test]
fn moving_past_the_last_tab_makes_a_new_one() {
    let ids = panes(2);
    let mut tabs = tabs_of(&[&ids[..]]);
    let first = tabs.tabs()[0].id;

    let new = tabs
        .move_pane(ids[1], Placement::NewAfter { tab: Some(first) })
        .expect("the move is allowed");

    assert_ne!(new, first);
    assert_eq!(layout(&tabs), vec![vec![ids[0]], vec![ids[1]]]);
}

#[test]
fn a_pane_alone_on_its_tab_is_already_on_a_new_one() {
    let ids = panes(1);
    let mut tabs = tabs_of(&[&ids[..]]);
    let first = tabs.tabs()[0].id;

    assert_eq!(
        tabs.move_pane(ids[0], Placement::NewAfter { tab: Some(first) }),
        Ok(first)
    );
    assert_eq!(tabs.tabs().len(), 1);
}

#[test]
fn moving_the_last_pane_off_a_tab_removes_the_tab() {
    let ids = panes(2);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..]]);
    let first = tabs.tabs()[0].id;

    tabs.move_pane(ids[1], Placement::Into { tab: first })
        .expect("there is room");

    assert_eq!(layout(&tabs), vec![ids.clone()]);
}

#[test]
fn a_name_is_cleaned_trimmed_and_capped() {
    let ids = panes(1);
    let mut tabs = tabs_of(&[&ids[..]]);
    let tab = tabs.tabs()[0].id;

    tabs.rename(tab, "  fix\u{7}  login\n ")
        .expect("the tab exists");
    assert_eq!(tabs.tabs()[0].name.as_deref(), Some("fix  login"));

    tabs.rename(tab, &"x".repeat(100)).expect("the tab exists");
    assert_eq!(
        tabs.tabs()[0]
            .name
            .as_ref()
            .map(|name| name.chars().count()),
        Some(NAME_LIMIT)
    );
}

#[test]
fn an_empty_name_goes_back_to_the_automatic_one() {
    let ids = panes(1);
    let mut tabs = tabs_of(&[&ids[..]]);
    let tab = tabs.tabs()[0].id;

    tabs.rename(tab, "work").expect("the tab exists");
    tabs.rename(tab, " \t ").expect("the tab exists");

    assert_eq!(tabs.tabs()[0].name, None);
}

#[test]
fn renaming_a_tab_that_is_gone_is_refused() {
    let mut tabs = ProjectTabs::new();

    assert_eq!(tabs.rename(TabId::new(), "work"), Err(TabError::NoSuchTab));
}

#[test]
fn a_tabs_members_are_what_closing_it_closes() {
    let ids = panes(3);
    let tabs = tabs_of(&[&ids[..2], &ids[2..]]);
    let first = tabs.tabs()[0].id;

    assert_eq!(tabs.members(first), Ok(ids[..2].to_vec()));
    assert_eq!(tabs.members(TabId::new()), Err(TabError::NoSuchTab));
}

#[test]
fn a_tab_moves_within_the_row_and_stops_at_its_end() {
    let ids = panes(3);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..2], &ids[2..]]);
    let first = tabs.tabs()[0].id;

    tabs.move_tab(first, 1).expect("the tab exists");
    assert_eq!(
        layout(&tabs),
        vec![vec![ids[1]], vec![ids[0]], vec![ids[2]]]
    );

    tabs.move_tab(first, 99).expect("the tab exists");
    assert_eq!(
        layout(&tabs),
        vec![vec![ids[1]], vec![ids[2]], vec![ids[0]]]
    );

    assert_eq!(tabs.move_tab(TabId::new(), 0), Err(TabError::NoSuchTab));
}

#[test]
fn the_full_message_names_the_capacity() {
    assert_eq!(TabError::Full.to_string(), "that tab is full (4 panes)");
    assert_eq!(TabError::NoSuchTab.to_string(), "that tab is gone");
}

#[test]
fn a_tab_that_is_gone_has_no_room() {
    assert!(ProjectTabs::new().is_full(TabId::new()));
}

#[test]
fn the_default_placement_is_auto() {
    assert_eq!(Placement::default(), Placement::Auto);
}
