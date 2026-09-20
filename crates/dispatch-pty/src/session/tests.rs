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
fn a_pty_is_not_finished_until_its_output_has_been_delivered() {
    // The exit and the output are separate events. Answering a delegation at
    // the exit sends a tail with none of the subagent's output in it, which is
    // what happens on Windows, where ConPTY's pipe lags the process object.
    let mut pty = Pty::spawn(&shell("echo finished-marker"), &cwd(), Size::new(80, 24))
        .expect("spawning succeeds");

    let mut output = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);

    while std::time::Instant::now() < deadline && !pty.is_finished() {
        output.extend_from_slice(&pty.drain());
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(
        pty.is_finished(),
        "the pseudoterminal should reach end-of-file once the child is gone"
    );
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
