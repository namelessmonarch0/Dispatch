//! Dispatch: an agent orchestration TUI.

mod app;
mod terminal;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, Event};
use dispatch_config::HarnessRegistry;
use dispatch_pty::Size;

use app::App;
use terminal::{TerminalGuard, install_panic_hook};

/// One control surface for several coding agents.
#[derive(Debug, Parser)]
#[command(name = "dispatch", version, about)]
struct Args {
    /// Projects to open. Defaults to the current directory.
    projects: Vec<PathBuf>,

    /// Write diagnostics to this file instead of the default location.
    #[arg(long)]
    log_file: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(args.log_file.clone())?;

    // Written on first run and never overwritten, so local edits survive.
    let harness_dir =
        dispatch_os::paths::harnesses_dir().context("failed to locate the harness directory")?;
    let written = dispatch_config::write_missing_built_ins(&harness_dir)
        .context("failed to write the built-in harnesses")?;
    if !written.is_empty() {
        tracing::info!(?written, "wrote built-in harnesses");
    }

    let harnesses = HarnessRegistry::load_from_dir(&harness_dir)
        .context("failed to load harness definitions")?;
    tracing::info!(count = harnesses.len(), "harnesses registered");

    let mut app = App::new(harnesses);

    let projects = if args.projects.is_empty() {
        vec![std::env::current_dir().context("failed to read the working directory")?]
    } else {
        args.projects
    };
    for project in projects {
        let root = project
            .canonicalize()
            .with_context(|| format!("no such directory: {}", project.display()))?;
        app.add_project(root);
    }

    // From here on the terminal belongs to Dispatch, so nothing may write to
    // stdout and every exit path has to restore it.
    install_panic_hook();
    let mut guard = TerminalGuard::acquire()?;

    run(&mut app, &mut guard)
}

/// The event loop.
///
/// Waits for input with a timeout rather than spinning, so an idle Dispatch
/// costs nothing, and coalesces pane output into at most one redraw per frame
/// so a chatty agent cannot starve the interface.
fn run(app: &mut App, guard: &mut TerminalGuard) -> Result<()> {
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut needs_draw = true;
    let mut last_area = Size::new(80, 24);

    while !app.should_quit() {
        if needs_draw {
            guard
                .terminal()
                .draw(|frame| app.draw(frame))
                .context("failed to draw")?;

            // Panes are sized from the layout the draw just produced, so this
            // follows it rather than guessing.
            app.resize_panes();

            last_draw = Instant::now();
            needs_draw = false;
        }

        let timeout = App::poll_timeout(last_draw);

        if event::poll(timeout).context("failed to poll for input")? {
            let event = event::read().context("failed to read input")?;

            if let Event::Resize(cols, rows) = event {
                last_area = Size::new(cols, rows);
            }

            app.handle(&event, last_area)?;
            needs_draw = true;
        }

        if app.poll_panes() {
            needs_draw = true;
        }
    }

    Ok(())
}

/// Sends diagnostics to a file.
///
/// A TUI owns the screen, so anything written to stdout or stderr would land
/// in the middle of the interface.
fn init_logging(override_path: Option<PathBuf>) -> Result<()> {
    use tracing_subscriber::EnvFilter;

    let path = match override_path {
        Some(path) => path,
        None => dispatch_os::paths::log_file().context("failed to locate the log file")?,
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;

    tracing_subscriber::fmt()
        .with_writer(file)
        .with_ansi(false)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    Ok(())
}
