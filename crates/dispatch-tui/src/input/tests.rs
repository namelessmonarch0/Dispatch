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
