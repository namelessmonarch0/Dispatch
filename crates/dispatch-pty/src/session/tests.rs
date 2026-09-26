//! Tests for pseudoterminal supervision.
//!
//! Every test drives a real child process through a real pseudoterminal. The
//! shell differs per platform, so the helpers below name one command and let
//! each platform spell it.

use super::*;

/// A [`Launch`] running `script` through the platform's shell.
fn shell(script: &str) -> Launch {
    if cfg!(windows) {
        Launch {
            command: "cmd.exe".into(),
            args: vec!["/c".into(), script.into()],
            env: Default::default(),
        }
    } else {
        Launch {
            command: "sh".into(),
            args: vec!["-c".into(), script.into()],
            env: Default::default(),
        }
    }
}

/// A [`Launch`] running an interactive shell that reads commands from input.
fn interactive_shell() -> Launch {
    if cfg!(windows) {
        Launch {
            command: "cmd.exe".into(),
            args: Vec::new(),
            env: Default::default(),
        }
    } else {
        Launch {
            command: "sh".into(),
            args: Vec::new(),
            env: Default::default(),
        }
    }
}

fn cwd() -> std::path::PathBuf {
    std::env::temp_dir()
}

/// Trims each line and drops trailing blanks so assertions name content only.
fn visible(session: &PtySession) -> Vec<String> {
    let text = session
        .terminal()
        .plain_text()
        .expect("formatting succeeds");
    let mut lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// Polls until `predicate` holds or the timeout elapses.
fn wait_until(
    session: &mut PtySession,
    timeout: Duration,
    predicate: impl Fn(&PtySession) -> bool,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        session.drain();
        if predicate(session) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

const TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn a_childs_output_reaches_the_screen() {
    let mut session = PtySession::spawn(&shell("echo hello"), &cwd(), Size::new(80, 24))
        .expect("the shell can be started");

    session.drain_until_exit(TIMEOUT);

    assert!(
        visible(&session).iter().any(|l| l.contains("hello")),
        "expected 'hello' on the screen, got {:?}",
        visible(&session)
    );
}

#[test]
fn an_exit_status_is_reported() {
    let mut session = PtySession::spawn(&shell("exit 3"), &cwd(), Size::new(80, 24))
        .expect("the shell can be started");

    assert_eq!(session.drain_until_exit(TIMEOUT), RunState::Exited(3));
}

#[test]
fn a_clean_exit_reports_zero() {
    let mut session = PtySession::spawn(&shell("exit 0"), &cwd(), Size::new(80, 24))
        .expect("the shell can be started");

    assert_eq!(session.drain_until_exit(TIMEOUT), RunState::Exited(0));
}

#[test]
fn a_session_starts_out_running() {
    let mut session = PtySession::spawn(&interactive_shell(), &cwd(), Size::new(80, 24))
        .expect("the shell can be started");

    assert_eq!(session.state(), RunState::Running);
    assert!(session.pid().is_some(), "a running child has a pid");
    session.terminate();
}

#[test]
fn escape_sequences_are_interpreted_rather_than_printed() {
    let mut session = PtySession::spawn(
        &shell("printf '\\033[31mred\\033[0m done'"),
        &cwd(),
        Size::new(80, 24),
    )
    .expect("the shell can be started");

    session.drain_until_exit(TIMEOUT);

    let lines = visible(&session);
    assert!(
        lines.iter().any(|l| l.contains("red done")),
        "expected styled text to render as plain content, got {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("[31m")),
        "escape sequences leaked into the screen: {lines:?}"
    );
}

#[test]
fn input_written_to_a_pane_reaches_the_child() {
    let mut session = PtySession::spawn(&interactive_shell(), &cwd(), Size::new(80, 24))
        .expect("the shell can be started");

    session
        .write(b"echo from-input\r")
        .expect("writing to the pane succeeds");

    let saw_it = wait_until(&mut session, TIMEOUT, |s| {
        visible(s).iter().any(|l| l.contains("from-input"))
    });

    assert!(
        saw_it,
        "expected the echoed text, got {:?}",
        visible(&session)
    );
    session.terminate();
}

#[test]
#[cfg(unix)]
fn the_child_sees_the_size_it_was_given() {
    // A child that redraws to the wrong width is the most visible possible
    // bug, so assert the size the child itself reports, not ours.
    let mut session = PtySession::spawn(&shell("stty size"), &cwd(), Size::new(100, 30))
        .expect("the shell can be started");

    session.drain_until_exit(TIMEOUT);

    let lines = visible(&session);
    assert!(
        lines.iter().any(|l| l.contains("30 100")),
        "expected the child to report 30 rows by 100 columns, got {lines:?}"
    );
}

#[test]
#[cfg(unix)]
fn a_resize_is_reported_to_the_child() {
    let mut session = PtySession::spawn(&interactive_shell(), &cwd(), Size::new(80, 24))
        .expect("the shell can be started");

    session.resize(Size::new(120, 40)).expect("resizable");
    assert_eq!(session.size(), Size::new(120, 40));

    session
        .write(b"stty size\r")
        .expect("writing to the pane succeeds");

    let saw_it = wait_until(&mut session, TIMEOUT, |s| {
        visible(s).iter().any(|l| l.contains("40 120"))
    });

    assert!(
        saw_it,
        "expected the child to see 40 rows by 120 columns, got {:?}",
        visible(&session)
    );
    session.terminate();
}

#[test]
fn resizing_to_the_current_size_is_a_no_op() {
    let mut session = PtySession::spawn(&interactive_shell(), &cwd(), Size::new(80, 24))
        .expect("the shell can be started");

    session.resize(Size::new(80, 24)).expect("no-op resize");
    assert_eq!(session.size(), Size::new(80, 24));
    session.terminate();
}

#[test]
fn terminating_a_session_stops_the_child() {
    let mut session = PtySession::spawn(&interactive_shell(), &cwd(), Size::new(80, 24))
        .expect("the shell can be started");

    session.terminate();

    let exited = wait_until(&mut session, TIMEOUT, |s| {
        matches!(s.state(), RunState::Exited(_))
    });

    assert!(exited, "the child should have exited after terminate()");
}

#[test]
fn a_missing_command_is_reported_rather_than_panicking() {
    let launch = Launch {
        command: "dispatch-no-such-binary".into(),
        args: Vec::new(),
        env: Default::default(),
    };

    let error = PtySession::spawn(&launch, &cwd(), Size::new(80, 24))
        .expect_err("a missing binary cannot be started");

    assert!(
        matches!(error, PtyError::Spawn { .. }),
        "expected a spawn error, got {error:?}"
    );
}

#[test]
fn a_large_burst_of_output_is_not_truncated() {
    // A build log arrives faster than the loop drains it, so the reader must
    // not drop anything when the channel backs up.
    let script = if cfg!(windows) {
        "for /l %i in (1,1,200) do @echo line%i"
    } else {
        "i=1; while [ $i -le 200 ]; do echo line$i; i=$((i+1)); done"
    };

    let mut session = PtySession::spawn(&shell(script), &cwd(), Size::new(80, 24))
        .expect("the shell can be started");

    session.drain_until_exit(TIMEOUT);

    // The screen only holds the last rows, so assert on the tail rather than
    // the whole sequence.
    let lines = visible(&session);
    assert!(
        lines.iter().any(|l| l.contains("line200")),
        "expected the final line of a long burst, got {:?}",
        lines.last()
    );
}

#[test]
fn a_bare_pty_hands_over_bytes() {
    // What the daemon uses: the child's output, with nothing parsing it.
    let mut pty = Pty::spawn(&shell("printf hello-from-a-pty"), &cwd(), Size::new(80, 24))
        .expect("the shell starts");

    let (state, output) = pty.drain_until_exit(Duration::from_secs(10));

    assert_eq!(state, RunState::Exited(0));
    assert!(
        String::from_utf8_lossy(&output).contains("hello-from-a-pty"),
        "expected the child's output, got {:?}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn a_bare_pty_notices_an_exit_while_draining() {
    let mut pty =
        Pty::spawn(&shell("exit 5"), &cwd(), Size::new(80, 24)).expect("the shell starts");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let _ = pty.drain();
        if pty.state() != RunState::Running {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(pty.state(), RunState::Exited(5));
}

#[test]
fn a_session_stops_asking_for_redraws_once_a_pane_has_exited() {
    // Drain reports change, and an exited pane changes once. Reporting it every
    // poll would have the interface redrawing forever over a dead pane.
    let mut session =
        PtySession::spawn(&shell("exit 0"), &cwd(), Size::new(80, 24)).expect("the shell starts");

    assert_eq!(
        session.drain_until_exit(Duration::from_secs(10)),
        RunState::Exited(0)
    );

    assert!(
        !session.drain(),
        "nothing changed, so nothing needs redrawing"
    );
    assert!(!session.drain(), "and it stays that way");
}

#[test]
fn a_pty_delivers_what_a_child_printed_after_reporting_its_exit() {
    // The exit and the end of the output are separate events. Anything that
    // reads a child's whole output -- a delegation's tail, say -- has to keep
    // draining after the exit, or it reports the answer with the answer
    // missing.
    let mut pty = Pty::spawn(&shell("echo finished-marker"), &cwd(), Size::new(80, 24))
        .expect("spawning succeeds");

    let mut output = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut exited = None;

    while std::time::Instant::now() < deadline {
        output.extend_from_slice(&pty.drain());

        if pty.is_finished() {
            break;
        }

        // Past the exit, keep draining for a moment: this is the daemon's rule,
        // and the only one available where end-of-file never comes.
        match (exited, pty.state()) {
            (None, RunState::Exited(_)) => exited = Some(std::time::Instant::now()),
            (Some(at), _) if at.elapsed() >= Duration::from_millis(250) => break,
            _ => {}
        }

        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(
        matches!(pty.state(), RunState::Exited(0)),
        "got {:?}",
        pty.state()
    );
    assert!(
        String::from_utf8_lossy(&output).contains("finished-marker"),
        "everything the child printed should have arrived by then, got {:?}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
#[cfg_attr(
    windows,
    ignore = "the pseudoconsole is held open on purpose, so the master never reaches end-of-file"
)]
fn a_finished_pty_is_one_whose_output_is_complete() {
    // `is_finished` is the honest signal: both senders gone means the reader
    // reached end-of-file and the waiter reported the exit. Windows cannot give
    // it -- `Pty` holds the slave so the pseudoconsole stays alive, which is what
    // keeps a child from writing into a dead console -- so there the daemon's
    // grace period is the only rule, and the test above is the one that covers
    // it.
    let mut pty = Pty::spawn(&shell("echo complete-marker"), &cwd(), Size::new(80, 24))
        .expect("spawning succeeds");

    let mut output = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);

    while std::time::Instant::now() < deadline && !pty.is_finished() {
        output.extend_from_slice(&pty.drain());
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(pty.is_finished(), "the reader should reach end-of-file");
    assert!(
        String::from_utf8_lossy(&output).contains("complete-marker"),
        "a finished pseudoterminal has delivered everything, got {:?}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn a_pane_is_told_about_dispatchs_terminal_not_the_one_outside() {
    // Dispatch draws the pane with its own emulator. A program told it is in
    // kitty would send kitty's private sequences through, and over SSH a
    // remote side without that terminfo mis-draws.
    let mut command = CommandBuilder::new("true");
    command.env("TERM", "xterm-kitty");
    command.env("KITTY_WINDOW_ID", "7");
    command.env("GHOSTTY_RESOURCES_DIR", "/usr/share/ghostty");
    // A multiplexer outside counts too: a program that sees `TMUX` wraps its
    // sequences for a tmux that is not the one drawing it.
    command.env("TMUX", "/tmp/tmux-1000/default,1234,0");

    apply_pane_env(&mut command);

    assert_eq!(
        command.get_env("TERM"),
        Some(std::ffi::OsStr::new("xterm-256color"))
    );
    assert_eq!(
        command.get_env("COLORTERM"),
        Some(std::ffi::OsStr::new("truecolor"))
    );
    assert_eq!(
        command.get_env("TERM_PROGRAM"),
        Some(std::ffi::OsStr::new("dispatch"))
    );
    assert_eq!(
        command.get_env("TERM_PROGRAM_VERSION"),
        Some(std::ffi::OsStr::new(env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(command.get_env("KITTY_WINDOW_ID"), None);
    assert_eq!(command.get_env("GHOSTTY_RESOURCES_DIR"), None);
    assert_eq!(command.get_env("TMUX"), None);
}

#[test]
#[cfg(unix)]
fn a_child_sees_dispatchs_terminal() {
    let mut session = PtySession::spawn(
        &shell("echo T=$TERM C=$COLORTERM P=$TERM_PROGRAM"),
        &cwd(),
        Size::new(80, 24),
    )
    .expect("the shell starts");

    assert!(wait_until(&mut session, TIMEOUT, |session| {
        visible(session)
            .iter()
            .any(|line| line.contains("T=xterm-256color C=truecolor P=dispatch"))
    }));
}

#[test]
#[cfg(unix)]
fn a_harnesss_own_environment_still_wins() {
    let mut launch = shell("echo T=$TERM");
    launch.env.insert("TERM".into(), "vt100".into());

    let mut session =
        PtySession::spawn(&launch, &cwd(), Size::new(80, 24)).expect("the shell starts");

    assert!(wait_until(&mut session, TIMEOUT, |session| {
        visible(session).iter().any(|line| line.contains("T=vt100"))
    }));
}
