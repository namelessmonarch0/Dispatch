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
            ..Default::default()
        }
    } else {
        Launch {
            command: "sh".into(),
            args: vec!["-c".into(), script.into()],
            env: Default::default(),
            ..Default::default()
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
            ..Default::default()
        }
    } else {
        Launch {
            command: "sh".into(),
            args: Vec::new(),
            env: Default::default(),
            ..Default::default()
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
        ..Default::default()
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
fn a_variable_the_launch_unsets_is_not_inherited() {
    // Removed from what the child would inherit from this process, not just
    // left unset by the launch: a stale value is exactly what inheriting
    // brings.
    let (variable, script, absent) = if cfg!(windows) {
        ("USERPROFILE", "echo [%USERPROFILE%]", "[%USERPROFILE%]")
    } else {
        ("HOME", "echo \"[${HOME-unset}]\"", "[unset]")
    };
    assert!(
        std::env::var_os(variable).is_some(),
        "{variable} is set here, so the child would inherit it"
    );
    let mut launch = shell(script);
    launch.unset.insert(variable.to_string());

    let mut pty = Pty::spawn(&launch, &cwd(), Size::new(80, 24)).expect("the shell starts");
    let (state, output) = pty.drain_until_exit(Duration::from_secs(10));

    assert_eq!(state, RunState::Exited(0));
    assert!(
        String::from_utf8_lossy(&output).contains(absent),
        "{variable} reached the child: {:?}",
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

/// Drains until `needle` has been printed, or panics.
#[cfg(unix)]
fn drain_until(pty: &mut Pty, needle: &str) -> Vec<u8> {
    let deadline = std::time::Instant::now() + TIMEOUT;
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains(needle) {
        assert!(
            std::time::Instant::now() < deadline,
            "never saw {needle:?}; saw {:?}",
            String::from_utf8_lossy(&seen)
        );
        seen.extend(pty.drain());
        std::thread::sleep(Duration::from_millis(10));
    }
    seen
}

#[test]
#[cfg(unix)]
fn a_pane_that_stops_reading_does_not_block_its_writer() {
    // Raw mode so the terminal buffers what it is sent rather than
    // processing lines, then never read: the shape of an agent busy with
    // something else when a paste arrives.
    let mut pty = Pty::spawn(
        &shell("stty raw -echo; echo READY; sleep 30"),
        &cwd(),
        Size::new(80, 24),
    )
    .expect("the shell starts");
    drain_until(&mut pty, "READY");

    // Larger than the budget, into an empty queue: accepted, and at once.
    let started = std::time::Instant::now();
    pty.write(&vec![b'x'; INPUT_BUDGET + 1])
        .expect("a paste into an empty queue is accepted whatever its size");
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "the write waited {:?} for a pane that is not reading",
        started.elapsed()
    );

    // Anything more is refused whole, and says why.
    let refused = pty.write(b"y");
    assert!(
        matches!(refused, Err(PtyError::InputFull { .. })),
        "expected the input to be full, got {refused:?}"
    );

    pty.terminate();
}

#[test]
#[cfg_attr(
    windows,
    ignore = "the pseudoconsole is held open on purpose, so its output never reaches end-of-file"
)]
fn a_finished_pty_is_one_whose_output_is_complete() {
    // `is_finished` is the honest signal: both senders gone means the reader
    // reached end-of-file and the waiter reported the exit. Windows cannot give
    // it -- `Pty` holds the pseudoconsole open, which is what keeps a child from
    // writing into a dead console -- so there the daemon's grace period is the
    // only rule, and the test above is the one that covers it.
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
#[cfg(unix)]
fn a_flood_is_handed_over_a_budget_at_a_time() {
    // A megabyte, enough to cross DRAIN_BUDGET (128 KiB) many times over,
    // printed as fast as the shell can and drained slowly. Before, the first
    // drain took whatever had piled up in an unbounded channel; now each
    // takes a bounded slice, and nothing is lost. The loop gets its own
    // deadline instead of the shared 10 s TIMEOUT: macOS CI runners push PTY
    // output slowly, and 30 s is generous enough to outlast that without
    // masking a real regression.
    const TOTAL: usize = 1_000_000;
    const LOOP_DEADLINE: Duration = Duration::from_secs(30);
    let mut pty = Pty::spawn(
        &shell(&format!("head -c {TOTAL} /dev/zero | tr '\\0' x")),
        &cwd(),
        Size::new(80, 24),
    )
    .expect("the shell starts");

    let deadline = std::time::Instant::now() + LOOP_DEADLINE;
    let mut received = 0;
    while received < TOTAL {
        assert!(
            std::time::Instant::now() < deadline,
            "only {received} of {TOTAL} bytes arrived"
        );
        std::thread::sleep(Duration::from_millis(50));
        let chunk = pty.drain();
        assert!(
            chunk.len() <= DRAIN_BUDGET + 8192,
            "one drain handed over {} bytes",
            chunk.len()
        );
        received += chunk.iter().filter(|&&b| b == b'x').count();
    }

    assert_eq!(received, TOTAL, "every byte arrived, none twice");
}

#[test]
fn a_drain_stops_at_its_budget_and_the_next_takes_the_rest() {
    // The flood test above cannot tell a drain that keeps to its budget from
    // one that takes everything: a real pane seldom has more than a budget
    // waiting at the moment it is drained. Here the channel is filled before
    // anything drains it. Chunks of 5000 bytes, so one of them crosses the
    // budget rather than landing on it.
    const CHUNK: usize = 5000;
    const CHUNKS: usize = 30;

    let (tx, events) = sync_channel(OUTPUT_CHUNKS);
    let mut sent = Vec::new();
    for i in 0..CHUNKS {
        let chunk = vec![u8::try_from(i).expect("few chunks"); CHUNK];
        sent.extend_from_slice(&chunk);
        tx.try_send(PtyEvent::Output(chunk))
            .expect("the channel has room for every chunk");
    }
    assert!(
        sent.len() > DRAIN_BUDGET + CHUNK,
        "more than one drain's worth is waiting"
    );

    let first = drain_from(&events, DRAIN_BUDGET).output;
    assert!(
        (DRAIN_BUDGET..=DRAIN_BUDGET + CHUNK).contains(&first.len()),
        "one drain handed over {} of {} bytes",
        first.len(),
        sent.len()
    );

    let second = drain_from(&events, DRAIN_BUDGET).output;
    assert_eq!(
        [first, second].concat(),
        sent,
        "the next drain hands over the rest, in order"
    );
}

/// A script that starts something and waits, on every platform: a pane
/// with a grandchild.
fn a_tree() -> &'static str {
    if cfg!(windows) {
        "ping -n 30 127.0.0.1 >nul"
    } else {
        "sleep 30 & sleep 30"
    }
}

/// How many processes below the pane `a_tree` starts.
fn tree_size() -> usize {
    if cfg!(windows) { 1 } else { 2 }
}

#[test]
fn terminating_a_pane_ends_everything_it_started() {
    let mut pty =
        Pty::spawn(&shell(a_tree()), &cwd(), Size::new(80, 24)).expect("the shell starts");
    let pid = pty.pid().expect("a running pane has a pid");

    let deadline = std::time::Instant::now() + TIMEOUT;
    while dispatch_os::process::descendants(pid).len() < tree_size() {
        assert!(
            std::time::Instant::now() < deadline,
            "the pane never started its children"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let everyone: Vec<u32> = std::iter::once(pid)
        .chain(dispatch_os::process::descendants(pid))
        .collect();

    pty.terminate();

    let deadline = std::time::Instant::now() + TIMEOUT;
    while everyone
        .iter()
        .any(|p| dispatch_os::process::is_running(*p))
        && std::time::Instant::now() < deadline
    {
        pty.drain();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        everyone
            .iter()
            .all(|p| !dispatch_os::process::is_running(*p)),
        "a process the pane started outlived it: {everyone:?}"
    );
}

/// A script that exits leaving a grandchild running, a moment after
/// starting it: long enough for a test to see both.
fn a_tree_that_outlives_its_shell() -> &'static str {
    if cfg!(windows) {
        // `start /b` runs the first ping beside cmd rather than waiting for
        // it.
        "start /b ping -n 30 127.0.0.1 >nul & ping -n 3 127.0.0.1 >nul"
    } else {
        // SIGHUP ignored, so the first sleep outlives its terminal's session
        // leader.
        "trap '' HUP; sleep 30 & sleep 2"
    }
}

#[test]
fn dropping_a_pane_that_has_exited_ends_what_it_left_running() {
    let mut pty = Pty::spawn(
        &shell(a_tree_that_outlives_its_shell()),
        &cwd(),
        Size::new(80, 24),
    )
    .expect("the shell starts");
    let pid = pty.pid().expect("a running pane has a pid");

    let deadline = std::time::Instant::now() + TIMEOUT;
    while dispatch_os::process::descendants(pid).len() < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "the pane never started its children"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let below = dispatch_os::process::descendants(pid);

    let deadline = std::time::Instant::now() + TIMEOUT;
    while pty.state() == RunState::Running {
        assert!(
            std::time::Instant::now() < deadline,
            "the pane never exited"
        );
        pty.drain();
        std::thread::sleep(Duration::from_millis(20));
    }
    let left: Vec<u32> = below
        .into_iter()
        .filter(|p| dispatch_os::process::is_running(*p))
        .collect();
    assert!(!left.is_empty(), "nothing outlived the pane's shell");

    drop(pty);

    let deadline = std::time::Instant::now() + TIMEOUT;
    while left.iter().any(|p| dispatch_os::process::is_running(*p))
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        left.iter().all(|p| !dispatch_os::process::is_running(*p)),
        "what an exited pane left running outlived the pane: {left:?}"
    );
}

#[test]
fn a_failed_resize_says_it_was_a_resize() {
    // Opening failures are reported as failures to start; only a resize
    // raises this, and a message about opening sends whoever reads it the
    // wrong way.
    let error = PtyError::Resize(anyhow::anyhow!("HRESULT 0x80070057"));

    assert!(
        error
            .to_string()
            .starts_with("failed to resize the pseudoterminal"),
        "{error}"
    );
}

#[test]
#[cfg(unix)]
fn a_dropped_pane_lets_go_of_a_terminal_something_else_still_holds() {
    // A process that leaves the pane's session -- so ending the pane does not
    // end it -- and prints to the terminal until printing fails, then says
    // so. On Linux printing fails only once nothing holds the terminal's
    // other side. macOS revokes the terminal from everyone as soon as the
    // pane's session leader exits, so there this passes either way -- and
    // the shell waits, so that a revoke before the first print cannot fail
    // the test before the pane is dropped. perl, because macOS has no
    // setsid(1); it gives up after 20 s regardless.
    let dir = std::env::temp_dir().join(format!("dispatch-pty-held-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir is writable");
    let gone = dir.join("gone");
    let script = format!(
        "perl -e 'use POSIX; exit 0 if fork; POSIX::setsid(); $SIG{{HUP}} = \"IGNORE\"; $| = 1; \
         for (1..400) {{ unless (print \"tick\\n\") {{ open(my $f, \">\", $ARGV[0]); print $f \"gone\"; exit 0 }} \
         select(undef, undef, undef, 0.05) }}' \"{}\"; sleep 30",
        gone.display()
    );

    let mut pty = Pty::spawn(&shell(&script), &cwd(), Size::new(80, 24)).expect("the shell starts");
    drain_until(&mut pty, "tick");
    drop(pty);

    let deadline = std::time::Instant::now() + TIMEOUT;
    while !gone.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let let_go = gone.exists();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        let_go,
        "the pane's side of the terminal was still open after the pane was dropped"
    );
}
