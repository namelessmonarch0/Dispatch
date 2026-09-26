//! Tests for input routing.

use super::*;

use dispatch_core::PaneId;

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn press_with(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

fn ctrl_a() -> Event {
    press_with(KeyCode::Char('a'), KeyModifiers::CONTROL)
}

fn moved(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn router() -> InputRouter {
    InputRouter::new()
}

#[test]
fn an_ordinary_key_goes_to_the_pane() {
    let mut router = router();

    assert_eq!(
        router.handle(&press(KeyCode::Char('x')), &[]),
        Action::SendKey(Key::Char('x'), Modifiers::NONE)
    );
}

#[test]
fn control_keys_reach_the_pane_unchanged() {
    // Ctrl-C must reach the agent; intercepting it would make agents
    // uninterruptible.
    let mut router = router();

    assert_eq!(
        router.handle(&press_with(KeyCode::Char('c'), KeyModifiers::CONTROL), &[]),
        Action::SendKey(
            Key::Char('c'),
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            }
        )
    );
}

#[test]
fn special_keys_reach_the_pane() {
    let mut router = router();

    assert_eq!(
        router.handle(&press(KeyCode::Enter), &[]),
        Action::SendKey(Key::Enter, Modifiers::NONE)
    );
    assert_eq!(
        router.handle(&press(KeyCode::Up), &[]),
        Action::SendKey(Key::Up, Modifiers::NONE)
    );
    assert_eq!(
        router.handle(&press(KeyCode::F(5)), &[]),
        Action::SendKey(Key::Function(5), Modifiers::NONE)
    );
}

#[test]
fn the_prefix_alone_produces_nothing_and_arms() {
    let mut router = router();

    assert_eq!(router.handle(&ctrl_a(), &[]), Action::None);
    assert!(router.is_armed(), "the prefix should arm the next key");
}

#[test]
fn a_command_runs_after_the_prefix() {
    let mut router = router();
    router.handle(&ctrl_a(), &[]);

    assert_eq!(
        router.handle(&press(KeyCode::Char('z')), &[]),
        Action::ToggleZoom
    );
    assert!(!router.is_armed(), "the prefix should disarm after one key");
}

#[test]
fn every_command_is_bound() {
    let cases = [
        ('n', Action::NewPane),
        ('x', Action::ClosePane),
        ('z', Action::ToggleZoom),
        ('p', Action::ProjectPicker),
        ('H', Action::HarnessManager),
        ('a', Action::Approvals),
        ('s', Action::ExpandChild),
        ('c', Action::CollapseChild),
        ('f', Action::ToggleFold),
        ('o', Action::OpenProject),
        ('m', Action::AddMachine),
        ('[', Action::Scrollback),
        ('q', Action::Quit),
        ('h', Action::FocusDirection(Direction::Left)),
        ('j', Action::FocusDirection(Direction::Down)),
        ('k', Action::FocusDirection(Direction::Up)),
        ('l', Action::FocusDirection(Direction::Right)),
    ];

    for (key, expected) in cases {
        let mut router = router();
        router.handle(&ctrl_a(), &[]);
        assert_eq!(
            router.handle(&press(KeyCode::Char(key)), &[]),
            expected,
            "prefix then {key:?}"
        );
    }
}

#[test]
fn the_prefix_twice_sends_the_prefix_itself() {
    // Otherwise there is no way to type Ctrl-a into an agent that wants it.
    let mut router = router();
    router.handle(&ctrl_a(), &[]);

    assert_eq!(
        router.handle(&ctrl_a(), &[]),
        Action::SendKey(
            Key::Char('a'),
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            }
        )
    );
    assert!(!router.is_armed());
}

#[test]
fn an_unbound_key_after_the_prefix_does_nothing() {
    // It must not fall through to the pane: a mistyped command would
    // otherwise run something inside an agent.
    let mut router = router();
    router.handle(&ctrl_a(), &[]);

    assert_eq!(router.handle(&press(KeyCode::Char('Q')), &[]), Action::None);
    assert!(!router.is_armed());
}

#[test]
fn a_command_key_without_the_prefix_reaches_the_pane() {
    let mut router = router();

    assert_eq!(
        router.handle(&press(KeyCode::Char('z')), &[]),
        Action::SendKey(Key::Char('z'), Modifiers::NONE),
        "z is only a command after the prefix"
    );
}

#[test]
fn a_custom_prefix_is_honoured() {
    let mut router = InputRouter::with_prefix(Prefix {
        code: 'b',
        modifiers: KeyModifiers::CONTROL,
    });

    // The default prefix is now just a key.
    assert_eq!(
        router.handle(&ctrl_a(), &[]),
        Action::SendKey(
            Key::Char('a'),
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            }
        )
    );

    router.handle(&press_with(KeyCode::Char('b'), KeyModifiers::CONTROL), &[]);
    assert!(router.is_armed());
}

#[test]
fn key_releases_are_ignored() {
    // Windows and the kitty protocol report releases; acting on them would
    // send every keystroke twice.
    let mut router = router();
    let mut event = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
    event.kind = KeyEventKind::Release;

    assert_eq!(router.handle(&Event::Key(event), &[]), Action::None);
}

#[test]
fn a_release_does_not_consume_an_armed_prefix() {
    let mut router = router();
    router.handle(&ctrl_a(), &[]);

    let mut release = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
    release.kind = KeyEventKind::Release;
    router.handle(&Event::Key(release), &[]);

    assert!(router.is_armed(), "a release should not disarm the prefix");
    assert_eq!(
        router.handle(&press(KeyCode::Char('z')), &[]),
        Action::ToggleZoom
    );
}

#[test]
fn moving_the_pointer_focuses_the_pane_under_it() {
    let left = PaneId::new();
    let right = PaneId::new();
    let panes = [
        (left, Rect::new(0, 0, 10, 10)),
        (right, Rect::new(10, 0, 10, 10)),
    ];

    let mut router = router();

    assert_eq!(router.handle(&moved(5, 5), &panes), Action::FocusPane(left));
    assert_eq!(
        router.handle(&moved(15, 5), &panes),
        Action::FocusPane(right)
    );
}

#[test]
fn the_pane_boundary_is_exact() {
    // Column 10 belongs to the right pane, not the left.
    let left = PaneId::new();
    let right = PaneId::new();
    let panes = [
        (left, Rect::new(0, 0, 10, 10)),
        (right, Rect::new(10, 0, 10, 10)),
    ];

    let mut router = router();

    assert_eq!(router.handle(&moved(9, 0), &panes), Action::FocusPane(left));
    assert_eq!(
        router.handle(&moved(10, 0), &panes),
        Action::FocusPane(right)
    );
}

#[test]
fn moving_outside_every_pane_changes_nothing() {
    // The sidebar is not a pane, and crossing it must not drop focus.
    let pane = PaneId::new();
    let panes = [(pane, Rect::new(10, 0, 10, 10))];

    let mut router = router();

    assert_eq!(router.handle(&moved(2, 2), &panes), Action::None);
}

#[test]
fn a_paste_is_delivered_as_text() {
    // Bracketed paste arrives whole; sending it key by key would let an agent
    // act on a half-pasted line.
    let mut router = router();

    assert_eq!(
        router.handle(&Event::Paste("two words".into()), &[]),
        Action::Paste("two words".into())
    );
}

fn ctrl_t() -> Event {
    press_with(KeyCode::Char('t'), KeyModifiers::CONTROL)
}

fn alt(code: KeyCode) -> Event {
    press_with(code, KeyModifiers::ALT)
}

fn mouse_down() -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton_::Left),
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn ctrl_t_enters_tab_mode_and_sends_nothing() {
    let mut router = router();

    assert_eq!(router.handle(&ctrl_t(), &[]), Action::None);
    assert_eq!(router.key_mode(), KeyMode::Tabs);
}

#[test]
fn every_tab_mode_key_is_bound_and_says_whether_the_mode_stays() {
    let cases = [
        (KeyCode::Char('n'), Action::NewTab, KeyMode::Normal),
        (KeyCode::Char('r'), Action::RenameTab, KeyMode::Normal),
        (KeyCode::Char('x'), Action::CloseTab, KeyMode::Normal),
        (KeyCode::Left, Action::PreviousTab, KeyMode::Tabs),
        (KeyCode::Char('h'), Action::PreviousTab, KeyMode::Tabs),
        (KeyCode::Right, Action::NextTab, KeyMode::Tabs),
        (KeyCode::Char('l'), Action::NextTab, KeyMode::Tabs),
        (KeyCode::Char('['), Action::MovePaneLeft, KeyMode::Tabs),
        (KeyCode::Char(']'), Action::MovePaneRight, KeyMode::Tabs),
        (KeyCode::Char('i'), Action::MoveTabLeft, KeyMode::Tabs),
        (KeyCode::Char('o'), Action::MoveTabRight, KeyMode::Tabs),
        (KeyCode::Char('3'), Action::SelectTab(2), KeyMode::Normal),
        (KeyCode::Tab, Action::LastTab, KeyMode::Normal),
        (KeyCode::Esc, Action::None, KeyMode::Normal),
        (KeyCode::Enter, Action::None, KeyMode::Normal),
    ];

    for (code, action, after) in cases {
        let mut router = router();
        router.handle(&ctrl_t(), &[]);

        assert_eq!(
            router.handle(&press(code), &[]),
            action,
            "tab mode then {code:?}"
        );
        assert_eq!(router.key_mode(), after, "the mode after {code:?}");
    }
}

#[test]
fn any_other_key_in_tab_mode_is_ignored_and_the_mode_stays() {
    // A stray key must neither reach a pane nor drop the user out of what
    // they were doing.
    let mut router = router();
    router.handle(&ctrl_t(), &[]);

    for event in [
        press(KeyCode::Char('q')),
        press(KeyCode::Char('z')),
        ctrl_a(),
        alt(KeyCode::Char('n')),
    ] {
        assert_eq!(router.handle(&event, &[]), Action::None);
    }
    assert_eq!(router.key_mode(), KeyMode::Tabs);
}

#[test]
fn a_paste_in_tab_mode_ends_it_and_goes_to_the_pane() {
    // A paste is text for the pane; left in tab mode, the user's next key
    // would be read as a tab command they never meant.
    let mut router = router();
    router.handle(&ctrl_t(), &[]);

    assert_eq!(
        router.handle(&Event::Paste("hi".into()), &[]),
        Action::Paste("hi".into())
    );
    assert_eq!(router.key_mode(), KeyMode::Normal);
}

#[test]
fn ctrl_t_twice_sends_ctrl_t_to_the_pane() {
    // Claude Code's task list and a shell's fzf both use it.
    let mut router = router();
    router.handle(&ctrl_t(), &[]);

    assert_eq!(
        router.handle(&ctrl_t(), &[]),
        Action::SendKey(
            Key::Char('t'),
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            }
        )
    );
    assert_eq!(router.key_mode(), KeyMode::Normal);
}

#[test]
fn a_click_ends_tab_mode() {
    let mut router = router();
    router.handle(&ctrl_t(), &[]);

    router.handle(&mouse_down(), &[]);

    assert_eq!(router.key_mode(), KeyMode::Normal);
}

#[test]
fn tab_mode_does_not_start_while_the_prefix_is_armed() {
    let mut router = router();
    router.handle(&ctrl_a(), &[]);

    assert_eq!(router.handle(&ctrl_t(), &[]), Action::None);
    assert_eq!(router.key_mode(), KeyMode::Normal);
}

#[test]
fn the_direct_alt_keys_reach_dispatch() {
    let cases = [
        (KeyCode::Char('n'), Action::NewPane),
        (KeyCode::Char('i'), Action::MoveTabLeft),
        (KeyCode::Char('o'), Action::MoveTabRight),
        (KeyCode::Left, Action::FocusOrTab(Direction::Left)),
        (KeyCode::Char('h'), Action::FocusOrTab(Direction::Left)),
        (KeyCode::Right, Action::FocusOrTab(Direction::Right)),
        (KeyCode::Char('l'), Action::FocusOrTab(Direction::Right)),
        (KeyCode::Up, Action::FocusDirection(Direction::Up)),
        (KeyCode::Char('k'), Action::FocusDirection(Direction::Up)),
        (KeyCode::Down, Action::FocusDirection(Direction::Down)),
        (KeyCode::Char('j'), Action::FocusDirection(Direction::Down)),
    ];

    for (code, expected) in cases {
        let mut router = router();
        assert_eq!(router.handle(&alt(code), &[]), expected, "Alt {code:?}");
    }
}

#[test]
fn an_alt_key_dispatch_does_not_bind_still_reaches_the_pane() {
    let mut router = router();

    assert_eq!(
        router.handle(&alt(KeyCode::Char('b')), &[]),
        Action::SendKey(
            Key::Char('b'),
            Modifiers {
                alt: true,
                ..Modifiers::NONE
            }
        )
    );
    assert!(matches!(
        router.handle(
            &press_with(KeyCode::Char('N'), KeyModifiers::ALT | KeyModifiers::SHIFT),
            &[]
        ),
        Action::SendKey(..)
    ));
}
