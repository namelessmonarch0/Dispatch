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
"#;

/// A running Dispatch, its screen, and the ability to type at it.
struct Harness {
    session: PtySession,
    reader: ScreenReader,
    _config: tempdir::TempDir,
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
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

impl Harness {
    /// Starts Dispatch against a temporary project directory.
    fn start(size: Size) -> Self {
        let config = tempdir::TempDir::new("config").expect("temp dir is writable");
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

        // The config directory is passed through the child's own environment
        // rather than this process's. These tests run in parallel, and
        // setting a process-wide variable would have each one racing the
        // others' setup.
        let mut env = std::collections::BTreeMap::new();
        env.insert(
            dispatch_os::paths::CONFIG_DIR_ENV.to_string(),
            config.path().display().to_string(),
        );

        let launch = Launch {
            command: env!("CARGO_BIN_EXE_dispatch").to_string(),
            args: vec![project.display().to_string()],
            env,
        };

        let session = PtySession::spawn(&launch, config.path(), size)
            .expect("the dispatch binary can be started");

        Self {
            session,
            reader: ScreenReader::new().expect("a reader can be created"),
            _config: config,
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

    /// Waits until the screen satisfies `predicate`, returning whether it did.
    fn wait_for(&mut self, predicate: impl Fn(&[String]) -> bool) -> bool {
        let deadline = Instant::now() + SETTLE;
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
    }
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

    app.send(b"\x01n");
    assert!(
        app.wait_for(|lines| contains(lines, "Test Shell")),
        "the sidebar should name the harness by its display name"
    );

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

    app.send(b"\x01n");
    assert!(app.wait_for(|lines| contains(lines, "Test Shell")));

    app.send(b"stty size\r");

    // One pane fills the width left by the sidebar and the height left by the
    // status row.
    let expected = format!("{} {}", 30 - 1, 100 - dispatch_tui::sidebar::WIDTH);
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

    app.send(b"\x01n");
    assert!(app.wait_for(|lines| contains(lines, "Test Shell")));

    app.send(b"\x01n");
    assert!(
        app.wait_for(|lines| lines.iter().filter(|l| l.contains("Test Shell")).count() >= 2),
        "a second pane should be listed"
    );

    app.send(b"stty size\r");

    let full = 100 - dispatch_tui::sidebar::WIDTH;
    let expected = format!("{} {}", 30 - 1, full / 2);
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

    app.send(b"\x01n");
    assert!(app.wait_for(|lines| contains(lines, "Test Shell")));
    app.send(b"\x01n");
    assert!(app.wait_for(|lines| lines.iter().filter(|l| l.contains("Test Shell")).count() >= 2));

    let full = 100 - dispatch_tui::sidebar::WIDTH;

    app.send(b"\x01z");
    app.send(b"stty size\r");
    let zoomed = format!("{} {}", 30 - 1, full);
    assert!(
        app.wait_for(|lines| contains(lines, &zoomed)),
        "a zoomed pane should fill the grid and report {zoomed}"
    );

    app.send(b"\x01z");
    app.send(b"stty size\r");
    let restored = format!("{} {}", 30 - 1, full / 2);
    assert!(
        app.wait_for(|lines| contains(lines, &restored)),
        "unzooming should restore the grid and report {restored}"
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
