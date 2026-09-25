//! Drives the real binary in a pseudoterminal and reads what it paints.
//!
//! Every other test covers a layer. This one covers the product: a keystroke
//! entering Dispatch, reaching an agent, and its output arriving back on
//! screen. Assertions are made against a rendered screen rather than the byte
//! stream, because ratatui interleaves cursor movement with content and
//! nothing in the stream is contiguous.

use std::io::Write;
use std::time::{Duration, Instant};

use dispatch_config::Launch;
use dispatch_pty::{PtySession, ScreenReader, Size};

/// Long enough for a debug-build start plus a shell prompt on a loaded CI box.
const SETTLE: Duration = Duration::from_secs(10);

/// A harness definition the test controls, written into a temporary config
/// directory so the developer's own harnesses are neither used nor disturbed.
const SHELL_HARNESS: &str = r#"
id = "aaashell"
display_name = "Test Shell"
command = "sh"
args = []

[task]
args = ["-c", "{task}"]
"#;

/// A running Dispatch, its screen, and the ability to type at it.
struct Harness {
    session: PtySession,
    reader: ScreenReader,
    /// Where this Dispatch's logs are, for a failing test to hand over.
    config_dir: std::path::PathBuf,
    /// Kept alive for as long as the Dispatch under test, when the test did not
    /// bring its own.
    _config: Option<tempdir::TempDir>,
}

/// Minimal scoped temporary directory, to avoid a dependency for one use.
mod tempdir {
    use std::path::{Path, PathBuf};

    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(label: &str) -> std::io::Result<Self> {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);

            let path = std::env::temp_dir().join(format!(
                "dispatch-e2e-{}-{label}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path)?;
            Ok(Self(path))
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            // DISPATCH_E2E_KEEP leaves the directory behind, logs included.
            // These tests drive whole processes, and the logs are usually the
            // only record of why one of them did the wrong thing.
            if std::env::var_os("DISPATCH_E2E_KEEP").is_some() {
                eprintln!("keeping {}", self.0.display());
                return;
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// A configuration directory and a project, shared by whatever runs against it.
///
/// Separate from the Harness so a daemon and the clients attaching to it can be
/// pointed at the same one, and so it can outlive a client.
struct Fixture {
    config: tempdir::TempDir,
    project: std::path::PathBuf,
}

impl Fixture {
    /// The label is short because a Unix socket address is limited to about a
    /// hundred bytes, and the daemon's endpoint lives in here.
    fn new(label: &str) -> Self {
        let config = tempdir::TempDir::new(label).expect("temp dir is writable");
        let project = config.path().join("project");
        std::fs::create_dir_all(&project).expect("temp dir is writable");

        // Dispatch writes its built-ins here on first run; adding one of our
        // own proves a user-registered harness is picked up, and sorts first
        // so `new pane` selects it.
        let harnesses = config.path().join("harnesses");
        std::fs::create_dir_all(&harnesses).expect("temp dir is writable");
        // The id must match the file stem, and sorting first is what makes
        // "new pane" choose it over the built-ins.
        std::fs::write(harnesses.join("aaashell.toml"), SHELL_HARNESS.trim())
            .expect("temp dir is writable");

        Self { config, project }
    }

    /// The environment that points a child at this configuration.
    ///
    /// Passed through the child's own environment rather than this process's:
    /// these tests run in parallel, and a process-wide variable would have each
    /// one racing the others' setup.
    fn env(&self) -> std::collections::BTreeMap<String, String> {
        let mut env = std::collections::BTreeMap::new();
        env.insert(
            dispatch_os::paths::CONFIG_DIR_ENV.to_string(),
            self.config.path().display().to_string(),
        );
        env
    }
}

/// Stops the daemon a client started for `fixture`, by the pid it recorded.
///
/// A client-started daemon outlives the client on purpose, so a test that starts
/// one has to clean it up or leave a process behind on the developer's machine.
fn stop_recorded_daemon(fixture: &Fixture) {
    let pid_file = fixture.config.path().join("dispatchd.pid");

    let Ok(contents) = std::fs::read_to_string(&pid_file) else {
        return;
    };
    let Ok(pid) = contents.trim().parse::<u32>() else {
        return;
    };

    if cfg!(windows) {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status();
    } else {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
}

/// A running `dispatchd`, killed when the test ends however it ends.
struct Daemon(std::process::Child);

impl Daemon {
    /// Starts a daemon named `"local"`, as the existing single-daemon tests
    /// expect: unnamed because until federation there was only ever one.
    fn start(fixture: &Fixture) -> Self {
        Self::start_named(fixture, "local")
    }

    /// Starts a daemon that reports itself as `device`, so more than one can
    /// be told apart in a sidebar that lists them side by side.
    fn start_named(fixture: &Fixture, device: &str) -> Self {
        // A sibling of the client binary. Cargo only defines CARGO_BIN_EXE_ for
        // the package under test, so the daemon is found by path — and it is
        // whatever the last build left there. Run these against the workspace
        // (`cargo test --workspace`, as CI does); `cargo test -p dispatch`
        // alone will happily test a stale daemon.
        let mut path = std::path::PathBuf::from(env!("CARGO_BIN_EXE_dispatch"));
        path.set_file_name(if cfg!(windows) {
            "dispatchd.exe"
        } else {
            "dispatchd"
        });
        assert!(
            path.exists(),
            "{} is missing; build the workspace first",
            path.display()
        );

        let child = std::process::Command::new(&path)
            .arg(&fixture.project)
            .arg("--device")
            .arg(device)
            .envs(fixture.env())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the dispatchd binary can be started");

        let daemon = Self(child);
        daemon.wait_until_listening(fixture);
        daemon
    }

    /// Blocks until the daemon accepts a connection, or explains why it never
    /// did.
    ///
    /// Without this a test that starts a daemon and then watches a client blames
    /// the client for a daemon that never came up — which is exactly how a CI
    /// failure read before this existed, while the daemon's own log was deleted
    /// with the fixture.
    fn wait_until_listening(&self, fixture: &Fixture) {
        let endpoint = fixture.config.path().join("dispatchd.sock");
        let deadline = Instant::now() + SETTLE;

        while Instant::now() < deadline {
            if dispatch_os::ipc::Connection::connect_to(&endpoint).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        panic!(
            "the daemon never listened on {}; its log says:\n{}",
            endpoint.display(),
            log_tail(&fixture.config.path().join("dispatchd.log"))
        );
    }
}

impl Daemon {
    /// Stops the daemon the way an operator would, so it takes its panes with
    /// it rather than orphaning them.
    fn stop(mut self) {
        #[cfg(unix)]
        let asked = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(self.0.id().to_string())
            .status()
            .is_ok_and(|s| s.success());
        #[cfg(not(unix))]
        let asked = false;

        if asked {
            let deadline = Instant::now() + SETTLE;
            while Instant::now() < deadline {
                if matches!(self.0.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Harness {
    /// Starts Dispatch against a temporary project directory, owning its agents.
    fn start(size: Size) -> Self {
        let fixture = Fixture::new("config");
        let mut harness = Self::spawn(&fixture, size, &[]);
        harness._config = Some(fixture.config);
        harness
    }

    /// Starts Dispatch attached to a daemon serving `fixture`.
    fn attached(fixture: &Fixture, size: Size) -> Self {
        Self::spawn(fixture, size, &["--attach".to_string()])
    }

    /// Starts Dispatch attached, refusing to start a daemon itself.
    fn attached_only(fixture: &Fixture, size: Size) -> Self {
        Self::spawn(
            fixture,
            size,
            &["--attach".to_string(), "--no-start".to_string()],
        )
    }

    /// Starts Dispatch against a directory other than the fixture's own,
    /// sharing its configuration — which is where the kept project list is.
    fn spawn_in(fixture: &Fixture, root: &std::path::Path, size: Size) -> Self {
        Self::spawn_with_root(fixture, root, size, &[])
    }

    fn spawn(fixture: &Fixture, size: Size, extra: &[String]) -> Self {
        Self::spawn_with_root(fixture, &fixture.project.clone(), size, extra)
    }

    fn spawn_with_root(
        fixture: &Fixture,
        root: &std::path::Path,
        size: Size,
        extra: &[String],
    ) -> Self {
        let mut args = vec![root.display().to_string()];
        args.extend_from_slice(extra);

        let launch = Launch {
            command: env!("CARGO_BIN_EXE_dispatch").to_string(),
            args,
            env: fixture.env(),
        };

        let session = PtySession::spawn(&launch, fixture.config.path(), size)
            .expect("the dispatch binary can be started");

        Self {
            session,
            reader: ScreenReader::new().expect("a reader can be created"),
            config_dir: fixture.config.path().to_path_buf(),
            _config: None,
        }
    }

    /// Types bytes at Dispatch.
    fn send(&mut self, bytes: &[u8]) {
        self.session.write(bytes).expect("writing succeeds");
    }

    /// The screen as trimmed lines.
    fn lines(&mut self) -> Vec<String> {
        self.session.drain();
        let screen = self
            .reader
            .read(self.session.terminal())
            .expect("the screen can be read");
        screen
            .text_lines()
            .into_iter()
            .map(|l| l.trim_end().to_string())
            .collect()
    }

    /// Opens the harness picker and chooses the test shell.
    ///
    /// The picker lists harnesses by display name, and the test shell sorts
    /// first, so Enter takes it.
    fn spawn_shell(&mut self) {
        let before = self.shell_panes();

        self.send(b"\x01n");
        assert!(
            self.wait_for(|lines| contains(lines, "New pane")),
            "the harness picker should open"
        );

        self.send(b"\r");
        assert!(
            self.wait_for(move |lines| panes_shown(lines) > before),
            "a pane should be listed after choosing a harness"
        );
    }

    /// How many panes the sidebar lists.
    fn shell_panes(&mut self) -> usize {
        panes_shown(&self.lines())
    }

    /// Selects, from the project picker, the project whose path names
    /// `machine`.
    ///
    /// Both fixtures' projects are literally named "project" -- only the
    /// path says which machine one is on -- so `spawn_shell` alone spawns on
    /// whichever project's `ProjectOpened` happened to be applied first, a
    /// race a federation test cannot let decide what it is proving. The
    /// picker draws its items in the pane area, to the right of the sidebar
    /// column, so slicing each line past that column keeps a device's own
    /// name in the sidebar from being mistaken for a picker row.
    fn select_project(&mut self, machine: &str) {
        self.send(b"\x01p");
        // " Project " (singular, spaced) rather than "Project": the sidebar's
        // own frame is titled "Projects" and would otherwise satisfy the
        // wait before the picker ever opened.
        assert!(
            self.wait_for(|lines| contains(lines, " Project ")),
            "the project picker should open"
        );

        let width = dispatch_tui::sidebar::WIDTH as usize;
        let lines = self.lines();
        let target = lines
            .iter()
            .map(|line| line.chars().skip(width).collect::<String>())
            .filter(|row| row.contains("project"))
            .position(|row| row.contains(machine))
            .unwrap_or_else(|| {
                panic!("the picker does not list a project on {machine}: {lines:?}")
            });

        for _ in 0..target {
            self.send(b"j");
        }
        self.send(b"\r");

        assert!(
            self.wait_for(|lines| !contains(lines, " Project ")),
            "choosing a project should close the picker"
        );
    }

    /// Waits until the screen satisfies `predicate`, returning whether it did.
    fn wait_for(&mut self, predicate: impl Fn(&[String]) -> bool) -> bool {
        self.wait_for_within(SETTLE, predicate)
    }

    /// As `wait_for`, for something that takes longer than a redraw: a
    /// machine joining is gated on a backoff measured in seconds.
    fn wait_for_within(
        &mut self,
        patience: Duration,
        predicate: impl Fn(&[String]) -> bool,
    ) -> bool {
        let deadline = Instant::now() + patience;
        loop {
            let lines = self.lines();
            if predicate(&lines) {
                return true;
            }
            if Instant::now() >= deadline {
                eprintln!("--- screen at timeout ---");
                for line in &lines {
                    eprintln!("{line}");
                }
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.session.terminate();

        // A screen dump says what the interface looked like; the logs say why.
        // Printed only when the test is already failing, and only on the way
        // out, because the fixture directory goes with it.
        if std::thread::panicking() {
            let dir = &self.config_dir;
            eprintln!(
                "--- dispatch.log ---\n{}",
                log_tail(&dir.join("dispatch.log"))
            );
            eprintln!(
                "--- dispatchd.log ---\n{}",
                log_tail(&dir.join("dispatchd.log"))
            );
        }
    }
}

/// The last lines of a log, or a note saying why there are none.
///
/// These tests drive whole processes, so a failure's explanation is usually in a
/// file that the fixture is about to delete.
fn log_tail(path: &std::path::Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(40);
            lines[start..].join("\n")
        }
        Err(error) => format!("({}: {error})", path.display()),
    }
}

/// Cells a pane's border takes off each axis.
///
/// Every pane is drawn inside a thin frame, so the terminal it runs is two
/// columns narrower and two rows shorter than the tile it was given.
const BORDER: u16 = 2;

/// What `stty size` should report for a tile of `cols` by `rows`.
fn reported_size(cols: u16, rows: u16) -> String {
    format!("{} {}", rows - BORDER, cols - BORDER)
}

/// How many pane rows the sidebar shows.
///
/// Counted by a row's indent, not by its label. A pane is named by whatever its
/// agent calls itself, and the shell this harness starts on Windows announces a
/// title the moment it is ready — so the harness's display name is on the screen
/// for a moment and gone, and counting it measured the shell rather than the
/// fleet.
///
/// Counted by the state glyph at the end of a row: every pane carries one and
/// nothing else in the list does. Indentation cannot tell them apart — a
/// project with no panes has a blank twisty, so its row starts with spaces
/// exactly as a pane's does.
///
/// The last row is the status line, which is not part of the sidebar.
fn panes_shown(lines: &[String]) -> usize {
    use dispatch_tui::sidebar::{CLOSED, DONE, FAILED, IDLE, RUNNING, STARTING};

    let width = dispatch_tui::sidebar::WIDTH as usize;
    let sidebar = lines.split_last().map_or(lines, |(_status, rest)| rest);

    sidebar
        .iter()
        .map(|line| line.chars().take(width).collect::<String>())
        .filter(|row| {
            [STARTING, RUNNING, IDLE, DONE, FAILED, CLOSED]
                .iter()
                .any(|glyph| row.contains(glyph))
        })
        .count()
}

/// Whether the sidebar — not a pane — shows `needle`.
///
/// A pane echoes what is typed at it, so a test that types a title and then
/// looks at the whole screen would pass whether or not the title was read.
fn sidebar_contains(lines: &[String], needle: &str) -> bool {
    let width = dispatch_tui::sidebar::WIDTH as usize;

    lines.iter().any(|line| {
        let column: String = line.chars().take(width).collect();
        column.contains(needle)
    })
}

fn contains(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn dispatch_starts_and_draws_its_interface() {
    let mut app = Harness::start(Size::new(100, 30));

    assert!(
        app.wait_for(|lines| contains(lines, "pane(s)")),
        "the status bar should be drawn"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn the_prefix_is_announced_so_a_keystroke_never_vanishes() {
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));

    app.send(b"\x01");

    assert!(
        app.wait_for(|lines| contains(lines, "PREFIX")),
        "an armed prefix must be visible"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn a_spawned_pane_runs_a_real_shell_that_echoes() {
    // The whole product in one test: a keystroke is encoded, reaches a child
    // through a pseudoterminal, and its output is emulated, laid out and
    // painted back.
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));

    app.spawn_shell();

    app.send(b"echo dispatch-end-to-end\r");
    assert!(
        app.wait_for(|lines| contains(lines, "dispatch-end-to-end")),
        "the shell's output should reach the screen"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn a_pane_is_told_the_size_of_the_rectangle_it_was_given() {
    // A child that redraws to the wrong size is the most visible bug the
    // layout can have, and only the child can confirm what it was told.
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));

    app.spawn_shell();

    app.send(b"stty size\r");

    // One pane fills the width left by the sidebar and the height left by the
    // top row and the status row, less its own border.
    let expected = reported_size(100 - dispatch_tui::sidebar::WIDTH, 30 - 2);
    assert!(
        app.wait_for(|lines| contains(lines, &expected)),
        "the child should report {expected}"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn a_second_pane_halves_the_width_of_the_first() {
    // Two panes cut down the middle, which is the specified layout, and both
    // children must learn their new size.
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));

    app.spawn_shell();
    app.spawn_shell();

    app.send(b"stty size\r");

    let full = 100 - dispatch_tui::sidebar::WIDTH;
    // The top row and the status row both come off the height, as above.
    let expected = reported_size(full / 2, 30 - 2);
    assert!(
        app.wait_for(|lines| contains(lines, &expected)),
        "with two panes the child should report {expected}"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn zoom_gives_a_pane_the_whole_grid_and_gives_it_back() {
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));

    app.spawn_shell();
    app.spawn_shell();

    let full = 100 - dispatch_tui::sidebar::WIDTH;

    app.send(b"\x01z");
    app.send(b"stty size\r");
    // The top row and the status row both come off the height, as above.
    let zoomed = reported_size(full, 30 - 2);
    assert!(
        app.wait_for(|lines| contains(lines, &zoomed)),
        "a zoomed pane should fill the grid and report {zoomed}"
    );

    app.send(b"\x01z");
    app.send(b"stty size\r");
    let restored = reported_size(full / 2, 30 - 2);
    assert!(
        app.wait_for(|lines| contains(lines, &restored)),
        "unzooming should restore the grid and report {restored}"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn a_project_opened_once_is_listed_on_the_next_start() {
    // The sidebar is the list of projects the user keeps, not the one
    // directory this process was pointed at.
    let fixture = Fixture::new("kept");
    let second = fixture.config.path().join("second");
    std::fs::create_dir_all(&second).expect("temp dir is writable");

    let mut first = Harness::spawn_in(&fixture, &fixture.project.clone(), Size::new(100, 30));
    assert!(first.wait_for(|lines| sidebar_contains(lines, "project")));
    first.send(b"\x01q");
    drop(first);

    let mut again = Harness::spawn_in(&fixture, &second, Size::new(100, 30));

    assert!(
        again.wait_for(
            |lines| sidebar_contains(lines, "second") && sidebar_contains(lines, "project")
        ),
        "both the new directory and the kept one are listed"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn the_browser_opens_beside_the_project_dispatch_was_started_in() {
    // A second project usually lives next to the first, so that is where
    // browsing starts rather than at whatever directory the shell was in.
    let fixture = Fixture::new("browse");
    // Both under a tree of their own, so the working directory Dispatch was
    // started in -- the config directory -- does not list the sibling.
    let tree = fixture.config.path().join("tree");
    let project = tree.join("opened");
    std::fs::create_dir_all(tree.join("sibling")).expect("temp dir is writable");
    std::fs::create_dir_all(&project).expect("temp dir is writable");

    let mut app = Harness::spawn_in(&fixture, &project, Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));

    app.send(b"\x01o");

    assert!(
        app.wait_for(|lines| contains(lines, "Open project") && contains(lines, "sibling")),
        "the browser lists the project's neighbours"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn quitting_restores_the_terminal() {
    // A Dispatch that exits without restoring leaves the user with a shell
    // that does not echo.
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));

    app.send(b"\x01q");

    let deadline = Instant::now() + SETTLE;
    loop {
        app.session.drain();
        if !matches!(app.session.state(), dispatch_pty::RunState::Running) {
            break;
        }
        assert!(Instant::now() < deadline, "dispatch did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }

    let _ = std::io::stdout().flush();
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn the_harness_picker_can_be_cancelled() {
    // Escape must leave nothing behind: a picker that dismissed but still
    // swallowed keys would look like Dispatch had frozen.
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));

    app.send(b"\x01n");
    assert!(app.wait_for(|lines| contains(lines, "New pane")));

    app.send(b"\x1b");
    assert!(
        app.wait_for(|lines| !contains(lines, "New pane")),
        "escape should close the picker"
    );

    // The keyboard is back with Dispatch rather than held by the picker.
    app.send(b"\x01");
    assert!(
        app.wait_for(|lines| contains(lines, "PREFIX")),
        "keys should reach Dispatch again after cancelling"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn the_project_picker_lists_the_open_project() {
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));

    app.send(b"\x01p");
    assert!(
        app.wait_for(|lines| contains(lines, "Project")),
        "the project picker should open"
    );

    app.send(b"\x1b");
}

#[test]
fn a_malformed_config_fails_the_start_before_anything_is_attached_to() {
    // Read late, a bad `config.toml` was only noticed once a daemon had been
    // started and every registered machine dialled, and they were left
    // running behind the error. `--no-start` stands in for that here: the
    // attach it asks for fails differently, so which error comes out says
    // which step ran first.
    let fixture = Fixture::new("badcfg");
    std::fs::write(
        fixture.config.path().join("config.toml"),
        "[interface\nmotion = true\n",
    )
    .expect("temp dir is writable");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dispatch"))
        .args(["--attach", "--no-start"])
        .arg(&fixture.project)
        .envs(fixture.env())
        .output()
        .expect("the dispatch binary runs");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(
        stderr.contains("config.toml"),
        "the configuration is what is reported: {stderr}"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn the_harness_manager_reports_when_nothing_needs_adding() {
    // The temporary configuration has every built-in written already, so the
    // manager has nothing to offer and must say so rather than opening an
    // empty box.
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));

    app.send(b"\x01H");
    assert!(
        app.wait_for(|lines| contains(lines, "registered")),
        "the manager should report what it found"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn a_picker_takes_the_keyboard_while_it_is_open() {
    // Arrow keys must choose rather than reach the agent underneath.
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));

    app.spawn_shell();
    app.send(b"\x01n");
    assert!(app.wait_for(|lines| contains(lines, "New pane")));

    app.send(b"jjj");
    app.send(b"\x1b");
    assert!(app.wait_for(|lines| !contains(lines, "New pane")));

    // A gap after Esc, deliberately: a terminal tells a bare Esc from the start
    // of an escape sequence by timing, so Esc followed immediately by `e` can be
    // read as Alt-e and swallow both. Real typing has this gap; a test writing
    // two buffers back to back does not.
    std::thread::sleep(Duration::from_millis(150));

    // Those keys went to the picker, so the shell never saw them.
    app.send(b"echo after-picker\r");
    assert!(
        app.wait_for(|lines| contains(lines, "after-picker")),
        "the pane should still be usable"
    );
    assert!(
        !contains(&app.lines(), "jjj"),
        "picker navigation must not reach the pane"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn a_pane_can_be_scrolled_back_and_typing_returns_to_the_newest_output() {
    // Scrollback is only useful if new output does not yank the view away and
    // typing brings it back, which is what every terminal does.
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));

    app.spawn_shell();

    // More output than the pane can show.
    app.send(b"i=1; while [ $i -le 80 ]; do echo row$i; i=$((i+1)); done\r");
    assert!(
        app.wait_for(|lines| contains(lines, "row80")),
        "the newest output should be visible first"
    );

    app.send(b"\x01[");
    assert!(
        app.wait_for(|lines| contains(lines, "scrolled back")),
        "scrolling back should be announced"
    );

    app.send(b"echo back-at-the-bottom\r");
    assert!(
        app.wait_for(|lines| contains(lines, "back-at-the-bottom")),
        "typing should return to the newest output"
    );
}

#[test]
fn attached_dispatch_runs_its_panes_in_the_daemon() {
    // The project is not passed to the client's own state: it arrives because
    // the daemon announced it, which is the only way an attached client can
    // learn an id it may spawn against.
    let fixture = Fixture::new("d1");
    let _daemon = Daemon::start(&fixture);
    let mut dispatch = Harness::attached(&fixture, Size::new(100, 30));

    assert!(
        dispatch.wait_for(|lines| contains(lines, "project")),
        "the daemon's project should be listed"
    );

    dispatch.spawn_shell();

    // Wait for the prompt before typing. A shell discards whatever is already
    // pending when it sets up the terminal, so type-ahead into a shell that has
    // not started yet is lost — here and in any other terminal.
    assert!(
        dispatch.wait_for(|lines| contains(lines, "$")),
        "the shell should print a prompt"
    );

    dispatch.send(b"echo attached-$((6*7))\r");
    assert!(
        dispatch.wait_for(|lines| contains(lines, "attached-42")),
        "the pane's output should come back through the daemon"
    );
}

#[test]
fn agents_survive_the_client_exiting() {
    // The whole point of the daemon: close the laptop, reattach, and the work
    // is still there.
    let fixture = Fixture::new("d2");
    let _daemon = Daemon::start(&fixture);

    let mut first = Harness::attached(&fixture, Size::new(100, 30));
    assert!(
        first.wait_for(|lines| contains(lines, "project")),
        "the daemon's project should be listed"
    );
    first.spawn_shell();
    assert!(
        first.wait_for(|lines| contains(lines, "$")),
        "the shell should print a prompt"
    );

    first.send(b"echo survivor-$((6*7))\r");
    assert!(
        first.wait_for(|lines| contains(lines, "survivor-42")),
        "the pane should answer the first client"
    );

    // Quit the client. The pane belongs to the daemon, so nothing is killed.
    first.send(b"\x01q");
    assert!(
        first.wait_for(|lines| panes_shown(lines) == 0),
        "the client should quit"
    );
    drop(first);

    let mut second = Harness::attached(&fixture, Size::new(100, 30));
    assert!(
        second.wait_for(|lines| panes_shown(lines) == 1),
        "a new client should be told about the pane that is still running"
    );

    // And it is shown what happened while it was not there, rather than a
    // blank rectangle.
    assert!(
        second.wait_for(|lines| contains(lines, "survivor-42")),
        "the new client should be replayed what the pane printed"
    );

    // And it is the same shell: it answers.
    second.send(b"echo reattached-$((6*7))\r");
    assert!(
        second.wait_for(|lines| contains(lines, "reattached-42")),
        "the surviving pane should still answer"
    );
}

#[test]
fn a_client_waits_for_a_daemon_that_is_restarted() {
    // A daemon being restarted should cost the view, not the session: the
    // client reattaches on its own and asks for its projects again.
    let fixture = Fixture::new("d3");
    let daemon = Daemon::start(&fixture);
    let mut dispatch = Harness::attached(&fixture, Size::new(100, 30));

    assert!(
        dispatch.wait_for(|lines| contains(lines, "project")),
        "the daemon's project should be listed"
    );
    dispatch.spawn_shell();

    daemon.stop();
    // Named, not "the daemon": the client holds one connection per machine, so
    // the notice has to say which one went quiet.
    assert!(
        dispatch.wait_for(|lines| contains(lines, "waiting for local")),
        "the client should say which daemon is gone rather than look alive"
    );

    let _restarted = Daemon::start(&fixture);
    assert!(
        dispatch.wait_for(|lines| contains(lines, "reattached")),
        "the client should reattach on its own"
    );

    // The panes went with the old daemon; the project comes back, and the
    // interface still works.
    assert!(
        dispatch.wait_for(|lines| panes_shown(lines) == 0 && contains(lines, "project")),
        "the view should be rebuilt from what the new daemon says"
    );
    dispatch.spawn_shell();
}

#[test]
fn a_pane_is_named_by_what_its_agent_calls_itself() {
    // An agent says what it is doing with a title sequence. The sidebar should
    // say that rather than the harness's name for the rest of the session.
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));
    app.spawn_shell();
    assert!(
        app.wait_for(|lines| contains(lines, "$")),
        "the shell should print a prompt"
    );

    // Short enough to survive the sidebar's width once the row's markers are
    // accounted for.
    app.send(b"printf '\\033]2;on-task\\007'\r");

    assert!(
        app.wait_for(|lines| sidebar_contains(lines, "on-task")),
        "the sidebar should show the title the child set"
    );
}

#[test]
fn an_attached_pane_is_named_by_its_agent_too() {
    // The title travels as ordinary output, so it costs the protocol nothing and
    // works the same on a pane the daemon owns.
    let fixture = Fixture::new("d4");
    let _daemon = Daemon::start(&fixture);
    let mut dispatch = Harness::attached(&fixture, Size::new(100, 30));

    assert!(
        dispatch.wait_for(|lines| contains(lines, "project")),
        "the daemon's project should be listed"
    );
    dispatch.spawn_shell();
    assert!(
        dispatch.wait_for(|lines| contains(lines, "$")),
        "the shell should print a prompt"
    );

    dispatch.send(b"printf '\\033]2;on-task\\007'\r");

    assert!(
        dispatch.wait_for(|lines| sidebar_contains(lines, "on-task")),
        "the sidebar should show the title the child set"
    );
}

#[test]
fn attaching_starts_a_daemon_when_none_is_listening() {
    // The daemon is meant to be an implementation detail: the user asked for
    // agents that outlive the interface, not for a second process to look after.
    let fixture = Fixture::new("d5");
    let mut dispatch = Harness::attached(&fixture, Size::new(100, 30));

    assert!(
        dispatch.wait_for(|lines| contains(lines, "project")),
        "a daemon should have been started and its project announced"
    );
    dispatch.spawn_shell();
    assert!(
        dispatch.wait_for(|lines| contains(lines, "$")),
        "the pane should be running in the daemon that was started"
    );

    // It recorded itself, which is how anything else finds it.
    let pid_file = fixture.config.path().join("dispatchd.pid");
    assert!(
        pid_file.is_file(),
        "the daemon should record its process id at {}",
        pid_file.display()
    );

    // And it outlives the client that started it: a second client finds it.
    drop(dispatch);
    let mut second = Harness::attached_only(&fixture, Size::new(100, 30));
    let found = second.wait_for(|lines| panes_shown(lines) == 1);

    stop_recorded_daemon(&fixture);
    assert!(
        found,
        "the daemon should still be serving the pane after its client exited"
    );
}

#[test]
fn no_start_refuses_rather_than_starting_a_daemon() {
    // For someone who runs their own daemon, and for scripts that would rather
    // fail than fork.
    let fixture = Fixture::new("d6");
    let mut dispatch = Harness::attached_only(&fixture, Size::new(100, 30));

    let said_so = dispatch.wait_for(|lines| contains(lines, "no daemon is listening"));
    stop_recorded_daemon(&fixture);

    assert!(
        said_so,
        "it should say what is wrong and how to fix it, rather than starting one"
    );
    assert!(
        !fixture.config.path().join("dispatchd.pid").is_file(),
        "and no daemon should have been started"
    );
}

#[test]
fn a_delegation_is_approved_by_hand_and_its_output_comes_back() {
    // The whole product promise in one test: an agent asks, a person says yes,
    // a second agent runs, and the first one reads the result.
    let fixture = Fixture::new("d7");
    let _daemon = Daemon::start(&fixture);
    let mut dispatch = Harness::attached(&fixture, Size::new(100, 30));

    assert!(dispatch.wait_for(|lines| contains(lines, "project")));
    dispatch.spawn_shell();
    assert!(
        dispatch.wait_for(|lines| contains(lines, "$")),
        "the parent shell should be ready"
    );

    dispatch.send(b"dispatch delegate \"echo delegated-$((6*7))\"\r");

    assert!(
        dispatch.wait_for(|lines| contains(lines, "wants to delegate")),
        "the approval prompt should open"
    );
    dispatch.send(b"a");

    assert!(
        dispatch.wait_for(|lines| panes_shown(lines) == 2),
        "the subagent should be listed under its parent"
    );
    assert!(
        dispatch.wait_for(|lines| contains(lines, "delegated-42")),
        "the parent pane should receive the subagent's output"
    );
}

#[test]
fn a_denied_delegation_runs_nothing_and_says_so() {
    let fixture = Fixture::new("d8");
    let _daemon = Daemon::start(&fixture);
    let mut dispatch = Harness::attached(&fixture, Size::new(100, 30));

    assert!(dispatch.wait_for(|lines| contains(lines, "project")));
    dispatch.spawn_shell();
    assert!(dispatch.wait_for(|lines| contains(lines, "$")));

    dispatch.send(b"dispatch delegate \"echo never-run\"\r");
    assert!(dispatch.wait_for(|lines| contains(lines, "wants to delegate")));

    dispatch.send(b"d");

    assert!(
        dispatch.wait_for(|lines| contains(lines, "denied")),
        "the agent should be told, in its own pane"
    );
    assert_eq!(
        panes_shown(&dispatch.lines()),
        1,
        "and nothing should have been started"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn two_daemons_share_one_sidebar() {
    // The slice in one test: two machines, one screen, and one of them going
    // down without taking the other with it.
    let here = Fixture::new("fed-a");
    let there = Fixture::new("fed-b");

    let first = Daemon::start_named(&here, "fed-a");
    let second = Daemon::start_named(&there, "fed-b");

    // Wide enough that the project picker, used below to select fed-a
    // explicitly, draws each item's full path rather than clipping it before
    // reaching the "fed-a"/"fed-b" that tells the two apart -- both fixtures'
    // projects are literally named "project", so the path is all a row has.
    let mut app = Harness::spawn(
        &here,
        Size::new(200, 30),
        &[
            "--attach".to_string(),
            "--daemon".to_string(),
            there
                .config
                .path()
                .join("dispatchd.sock")
                .display()
                .to_string(),
        ],
    );

    assert!(
        app.wait_for(|lines| sidebar_contains(lines, "fed-a") && sidebar_contains(lines, "fed-b")),
        "both machines are listed"
    );

    second.stop();

    assert!(
        app.wait_for(|lines| sidebar_contains(lines, "unreachable")),
        "the machine that went down says so"
    );
    assert!(
        app.wait_for(|lines| sidebar_contains(lines, "fed-a")),
        "and the other one is still there"
    );

    // "Still there" in the sidebar is not proof of much: that text was
    // painted before `second.stop()` too, and nothing would have removed it.
    // Running a real round trip through fed-a's own connection is the proof
    // that survives a dead sibling: a keystroke reaching a shell and its
    // output coming back, the same shape as every other pane test. Selected
    // explicitly rather than left to whatever `spawn_shell` would otherwise
    // spawn on: the selection follows whichever machine's `ProjectOpened`
    // happened to arrive first, and fed-b's landing first would spawn this
    // shell on the very machine just killed.
    app.select_project("fed-a");
    app.spawn_shell();
    app.send(b"echo fed-a-survives\r");
    assert!(
        app.wait_for(|lines| contains(lines, "fed-a-survives")),
        "the surviving machine should still run real work"
    );

    first.stop();
}

/// The `dispatchd` binary beside the client binary under test.
///
/// Cargo only defines `CARGO_BIN_EXE_` for the package under test, so the
/// daemon is found by path next to it rather than through its own env var.
fn dispatchd_binary() -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_BIN_EXE_dispatch"));
    path.set_file_name(if cfg!(windows) {
        "dispatchd.exe"
    } else {
        "dispatchd"
    });
    path
}

/// Kills the bridge processes talking to `endpoint`, leaving the daemon behind
/// them running.
///
/// Killing the transport rather than the daemon is the point: what the test
/// proves is that the agents were never the transport's to lose. Matched on
/// the endpoint path so a bridge belonging to another test running in parallel
/// is left alone.
///
/// A plain `pkill -f "--endpoint <path>"` is not enough here: the Dispatch
/// client under test was itself started with `--daemon-command "dispatchd
/// --stdio --endpoint <path>"`, so that same substring sits inside *its own*
/// command line too, and a bare pattern match kills the client under test
/// along with the bridge. Every matching process is checked against `ps` and
/// only ones actually named `dispatchd` — the bridge, never the client named
/// `dispatch` — are signalled.
#[cfg(unix)]
fn kill_bridge_to(endpoint: &std::path::Path) {
    let pattern = format!("--endpoint {}", endpoint.display());

    let Ok(output) = std::process::Command::new("pgrep")
        .args(["-f", "--", &pattern])
        .output()
    else {
        return;
    };

    for pid in String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
    {
        // The file name rather than a suffix match: `ends_with("dispatchd")`
        // would just as happily kill a `my-dispatchd` on the same box.
        let is_bridge = std::process::Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
            .is_ok_and(|out| {
                std::path::Path::new(String::from_utf8_lossy(&out.stdout).trim())
                    .file_name()
                    .is_some_and(|name| name == "dispatchd")
            });

        if is_bridge {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    }
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn a_machine_reached_over_a_bridge_outlives_its_transport() {
    // The slice's claim: the transport can die without the agents dying,
    // because the agents were never the transport's.
    let here = Fixture::new("stdio-a");
    let there = Fixture::new("stdio-b");

    let near = Daemon::start_named(&here, "near");
    let far = Daemon::start_named(&there, "far");

    let bridge = format!(
        "{} --stdio --endpoint {}",
        dispatchd_binary().display(),
        there.config.path().join("dispatchd.sock").display()
    );

    let mut app = Harness::spawn(
        &here,
        Size::new(200, 30),
        &[
            "--attach".to_string(),
            "--daemon-command".to_string(),
            bridge,
        ],
    );

    assert!(
        app.wait_for(|lines| sidebar_contains(lines, "near") && sidebar_contains(lines, "far")),
        "both machines are listed"
    );

    app.select_project("far");
    app.spawn_shell();
    app.send(b"echo over-the-bridge\r");
    assert!(
        app.wait_for(|lines| contains(lines, "over-the-bridge")),
        "a pane on the far machine echoes through the bridge"
    );

    // Kill the bridge child, not the daemon behind it.
    kill_bridge_to(&there.config.path().join("dispatchd.sock"));

    assert!(
        app.wait_for(|lines| sidebar_contains(lines, "far")),
        "the machine is still listed after its transport died"
    );

    // A keystroke sent to a connection that is down is dropped rather than
    // queued (out-of-order input reaching an agent minutes late is worse than
    // input that never arrives), and the row above is on screen throughout
    // the outage rather than only once the client has redialled. So typing
    // has to wait for the redial to actually land, or it races a client that
    // has not yet reconnected and is lost for good.
    assert!(
        app.wait_for(|lines| contains(lines, "reattached to far")),
        "the client should redial the bridge on its own"
    );
    app.send(b"echo still-here\r");
    assert!(
        app.wait_for(|lines| contains(lines, "still-here")),
        "and the pane still answers once the client has redialled"
    );

    near.stop();
    far.stop();
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn a_machine_asleep_at_startup_joins_when_it_wakes() {
    // The slice's claim: a registered machine that does not answer at
    // startup is drawn anyway, retried in the background, and joins — with
    // its kept project — once it does. No `--attach`: a registered machine
    // implies it.
    let here = Fixture::new("wake-a");
    let there = Fixture::new("wake-b");

    // The flag stands in for the machine being asleep. `--stdio` would
    // otherwise start the far daemon itself on the very first dial.
    let flag = there.config.path().join("awake");
    let script = format!(
        "test -e {} && exec {} --stdio --endpoint {}",
        flag.display(),
        dispatchd_binary().display(),
        there.config.path().join("dispatchd.sock").display()
    );
    std::fs::write(
        here.config.path().join("machines.toml"),
        format!(
            "[[machine]]\nname = \"tower\"\ntarget = \"tower\"\n\
             command = {{ program = \"sh\", args = [\"-c\", {script:?}] }}\n"
        ),
    )
    .expect("temp dir is writable");
    std::fs::write(
        here.config.path().join("projects.toml"),
        format!(
            "[machines.tower]\nroots = [{:?}]\n",
            there.project.display().to_string()
        ),
    )
    .expect("temp dir is writable");

    let mut app = Harness::spawn(&here, Size::new(200, 30), &[]);

    assert!(
        app.wait_for(
            |lines| sidebar_contains(lines, "tower") && sidebar_contains(lines, "unreachable")
        ),
        "the machine is drawn before it answers"
    );

    std::fs::write(&flag, b"").expect("temp dir is writable");

    assert!(
        app.wait_for_within(Duration::from_secs(45), |lines| contains(
            lines,
            "connected to tower"
        )),
        "it joins once it wakes"
    );

    app.select_project("tower");
    app.spawn_shell();
    app.send(b"echo woke-up\r");
    assert!(
        app.wait_for(|lines| contains(lines, "woke-up")),
        "and its kept project runs real work"
    );

    drop(app);
    stop_recorded_daemon(&here);
    stop_recorded_daemon(&there);
}
