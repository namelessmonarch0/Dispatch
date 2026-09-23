//! Dispatch: an agent orchestration TUI.

mod app;
mod approval;
mod backend;
mod delegate;
mod terminal;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, Event};
use dispatch_config::HarnessRegistry;
use dispatch_pty::Size;

use app::App;
use terminal::{TerminalGuard, install_panic_hook};

/// What the daemon logs this client as.
const CLIENT_NAME: &str = "dispatch";

/// How long to wait for a daemon this client started to answer.
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(15);

/// One control surface for several coding agents.
#[derive(Debug, Parser)]
#[command(name = "dispatch", version, about)]
struct Args {
    /// Projects to open. Defaults to the current directory.
    projects: Vec<PathBuf>,

    /// Write diagnostics to this file instead of the default location.
    #[arg(long)]
    log_file: Option<PathBuf>,

    /// Use the agents owned by `dispatchd` instead of starting them here, so
    /// they survive this process exiting. Starts a daemon if none is listening.
    #[arg(long)]
    attach: bool,

    /// With `--attach`, fail rather than starting a daemon when none is
    /// listening.
    #[arg(long, requires = "attach")]
    no_start: bool,

    /// Also attach to a daemon listening on this endpoint. Repeatable.
    ///
    /// The plumbing federation is built on. Dialled in the background and
    /// retried until it answers.
    #[arg(long = "daemon", value_name = "ENDPOINT")]
    daemons: Vec<PathBuf>,

    /// Also attach to the daemon a command speaks for. Repeatable.
    ///
    /// Split on whitespace, with no shell: every command this is for —
    /// `ssh user@host dispatchd --stdio`, a wrapper, an absolute path — is
    /// whitespace-separated. A program whose path contains a space needs the
    /// machine registry, which holds the program and its arguments apart.
    /// Dialled in the background and retried until it answers.
    #[arg(long = "daemon-command", value_name = "COMMAND")]
    daemon_commands: Vec<String>,

    /// Subcommands. Absent means run the interface.
    #[command(subcommand)]
    command: Option<Command>,
}

/// What to do instead of drawing an interface.
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Ask Dispatch to run one task in a second agent, and wait for it.
    ///
    /// Runs inside a Dispatch pane. Prints the subagent's output on stdout and
    /// progress on stderr, and exits with the subagent's own status: 69 when no
    /// daemon is listening, 75 when the request timed out or the connection
    /// dropped or the daemon does not know the asking pane or its subagent was
    /// killed before it could exit, 77 when it was denied, 78 when it was
    /// refused.
    Delegate {
        /// Which harness to run. Defaults to this pane's own.
        #[arg(long)]
        harness: Option<String>,

        /// Size to start the subagent at, as COLSxROWS.
        #[arg(long, default_value = "80x24", value_parser = parse_size)]
        size: (u16, u16),

        /// What the subagent should do.
        task: String,
    },
}

/// Parses `COLSxROWS`.
fn parse_size(text: &str) -> Result<(u16, u16), String> {
    let (cols, rows) = text
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected COLSxROWS, got {text:?}"))?;

    Ok((
        cols.parse().map_err(|_| format!("bad width {cols:?}"))?,
        rows.parse().map_err(|_| format!("bad height {rows:?}"))?,
    ))
}

fn main() -> Result<ExitCode> {
    let args = Args::parse();
    init_logging(args.log_file.clone())?;

    if let Some(Command::Delegate {
        harness,
        size,
        task,
    }) = args.command
    {
        return delegate::run(harness, size, &task);
    }

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

    // Resolved before anything is attached to or started, so a mistyped path
    // fails here rather than after a daemon has been spawned for it.
    let projects = if args.projects.is_empty() {
        vec![std::env::current_dir().context("failed to read the working directory")?]
    } else {
        args.projects.clone()
    };
    let asked_for: Vec<PathBuf> = projects
        .iter()
        .map(|project| {
            dispatch_os::paths::resolve(project)
                .with_context(|| format!("no such directory: {}", project.display()))
        })
        .collect::<Result<_>>()?;

    let config_dir = dispatch_os::paths::config_dir().context("failed to locate the config dir")?;

    // Read before deciding how to run: any registered machine means the
    // agents belong to daemons, this machine's included.
    let machines = dispatch_config::machines::load(&config_dir)
        .context("failed to read the registered machines")?;

    // The fleet is not one directory: the projects kept from earlier runs are
    // opened alongside whatever this one names, and a directory that has since
    // been deleted or moved is skipped rather than taking the start down.
    let mut roots: Vec<PathBuf> = dispatch_config::projects::load(&config_dir)
        .context("failed to read the kept projects")?
        .into_iter()
        .filter(|root| {
            if root.is_dir() {
                return true;
            }
            tracing::warn!(root = %root.display(), "kept project is missing; skipping it");
            false
        })
        .collect();

    for root in asked_for {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }

    use dispatch_client::{Client, Dial, Liveness};
    use dispatch_proto::Role;

    let mut app = if args.attach || !machines.is_empty() {
        // A registered machine implies attaching: standalone agents are this
        // process's children and cannot share a sidebar with a daemon's.
        // Fails before the terminal is taken over, so the reason is readable.
        let client = attach(&roots, args.no_start)?;
        tracing::info!(device = client.device(), "attached to a daemon");
        client.subscribe();
        App::attached(harnesses, client)
    } else {
        App::new(harnesses)
    };

    for machine in &machines {
        // Not `args`: that is the command line's own, still read below.
        let (program, arguments) = machine.dial();
        let client = Client::dial(
            Role::Interface,
            CLIENT_NAME,
            Liveness::default(),
            Dial::Command {
                program,
                args: arguments,
            },
        );
        client.subscribe();

        let roots = dispatch_config::projects::load_on(&config_dir, &machine.name)
            .context("failed to read the kept projects")?;
        app.attach_named(client, Some(machine.name.clone()), roots);
    }

    // Dialled in the background: the interface is drawn at once, and each
    // machine's row lights up when it answers. Attaching in turn cost thirty
    // seconds per machine that was down, before anything was on screen.
    for endpoint in &args.daemons {
        let client = Client::dial(
            Role::Interface,
            CLIENT_NAME,
            Liveness::default(),
            Dial::Endpoint(endpoint.clone()),
        );
        client.subscribe();
        app.attach_named(client, None, Vec::new());
    }

    for command in &args.daemon_commands {
        let mut words = command.split_whitespace().map(std::ffi::OsString::from);
        let Some(program) = words.next() else {
            app.set_status("--daemon-command was empty".to_string());
            continue;
        };

        let client = Client::dial(
            Role::Interface,
            CLIENT_NAME,
            Liveness::default(),
            Dial::Command {
                program,
                args: words.collect(),
            },
        );
        client.subscribe();
        app.attach_named(client, None, Vec::new());
    }

    // Set before the projects are added, so opening one is what keeps it.
    app.keep_projects_in(&config_dir);

    // Browsing starts next to the project Dispatch was pointed at: a second
    // project usually lives beside the first, not in whatever directory the
    // shell happened to be in.
    if let Some(parent) = roots.last().and_then(|root| root.parent()) {
        app.browse_from(parent);
    }

    for root in roots {
        app.add_project(root);
    }

    // From here on the terminal belongs to Dispatch, so nothing may write to
    // stdout and every exit path has to restore it.
    install_panic_hook();
    let mut guard = TerminalGuard::acquire()?;

    run(&mut app, &mut guard)?;
    Ok(ExitCode::SUCCESS)
}

/// Attaches to a daemon, starting one if nothing is listening.
///
/// Starting one is the point of the daemon being an implementation detail: the
/// user asked for agents that outlive the interface, not for a second process to
/// look after. `--no-start` is for the case where they do want to look after it
/// themselves, and for scripts that would rather fail than fork.
fn attach(roots: &[PathBuf], no_start: bool) -> Result<dispatch_client::Client> {
    use dispatch_client::{Client, ClientError};

    match Client::attach(CLIENT_NAME) {
        Ok(client) => return Ok(client),
        Err(ClientError::NotRunning(endpoint)) if no_start => {
            anyhow::bail!("no daemon is listening on {endpoint}; start one with `dispatchd`")
        }
        Err(ClientError::NotRunning(_)) => {}
        Err(error) => return Err(error).context("failed to attach to the daemon"),
    }

    let program = daemon_program()?;
    let args: Vec<std::ffi::OsString> = roots.iter().map(Into::into).collect();
    let pid = dispatch_os::process::spawn_detached(&program, &args)
        .with_context(|| format!("failed to start {}", program.display()))?;
    tracing::info!(pid, program = %program.display(), "started a daemon");

    // Binding, loading harnesses and opening a socket take a moment, and on a
    // cold start the daemon binary may still be paging in.
    let deadline = Instant::now() + DAEMON_START_TIMEOUT;
    let mut last = None;

    while Instant::now() < deadline {
        match Client::attach(CLIENT_NAME) {
            Ok(client) => return Ok(client),
            Err(error) => last = Some(error),
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let log = dispatch_os::paths::daemon_log_file()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "the daemon log".into());
    let reason = last.map_or_else(|| "it never answered".to_string(), |e| e.to_string());

    anyhow::bail!("started a daemon (pid {pid}) but could not attach: {reason}; see {log}")
}

/// Where to find the daemon binary.
///
/// Beside this one first: a Dispatch run from a build directory or an unpacked
/// archive should use the daemon it shipped with, not whichever one is on PATH.
fn daemon_program() -> Result<PathBuf> {
    let name = if cfg!(windows) {
        "dispatchd.exe"
    } else {
        "dispatchd"
    };

    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name(name);
        if sibling.is_file() {
            return Ok(sibling);
        }
    }

    Ok(PathBuf::from(name))
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

        // A daemon's panes arrive as messages rather than from a pseudoterminal.
        if app.poll_daemon() {
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
